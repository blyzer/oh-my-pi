# Native architecture and active-resource optimization research

OMP2 pin: [`fda1315052b8b4074f9ea5acc842b2cc0b08b2b2`](https://github.com/blyzer/oh-my-pi/tree/fda1315052b8b4074f9ea5acc842b2cc0b08b2b2).
Audit date: 2026-11-24. Publication authorized by the owner on 2026-11-25.
Research only; no production changes or accepted ADR edits. Findings remain
pinned to the audit revision above, not subsequent changes on `omp2`.

## Executive decision

Keep OMP2's co-built native Rust composition, existing controlled registries,
killable embedded-CPython extension/Eval processes, journal/CAS, typed protocols,
provider stack and verification authority. Optimize their active work and retained
resources before introducing an ABI, serialization framework or execution boundary.
The objective is a feature-rich binary with a small **active** footprint, not a
Saetta-sized executable. Static linking does not imply eager initialization or
physical residency of every linked page.

The highest-value **source-demonstrated opportunities**, whose end-to-end gains
remain to be measured, are:

1. **Read only the requested artifact range.**
   `BlobStoreAuthority::read_range` currently loads the complete blob and returns
   a `Bytes` slice retaining its allocation (`crates/envd/src/tool_url/artifact.rs:143–161`;
   `crates/journal/src/blob.rs:616–622`). Existing `open_range` is the starting
   point, not a new data plane. Preserve strict size/range/integrity semantics,
   particularly empty ranges versus its zero-length-means-remainder convention.
2. **Bound retained provider-attempt diagnostic records.**
   `HttpTransport.captures` is an `Arc<Mutex<Vec<_>>>` shared across transport clones;
   each response adds a record (`crates/ai/src/transport/http.rs:355–379,807–816`).
   Per-attempt bounds do not establish a bound on record count. Measure actual
   lifetime and payload retention before calling this a quantified leak; retain
   mandatory evidence through existing durable authority rather than dropping it.
3. **Render the same prompt bands once per request.**
   `system_items` and `volatile_items` each render the bands that the production
   `Kernel::project_request` requests together (`crates/agent/src/prompt/mod.rs:39–71`;
   `crates/agent/src/loop.rs:2517–2551`). Prefer request-scoped reuse over a new
   persistent context cache with difficult invalidation.
4. **Avoid rematerializing an already contiguous Bedrock body when signing.**
   The transport collects a reader into `BytesMut` for signing even when a codec
   supplied `BodySource::Bytes` (`crates/ai/src/transport/http.rs:677–698,1138–1165`;
   `crates/ai/src/codec/bedrock.rs:163–180,656–658`). Preserve exact signed bytes,
   retry/body evidence, bounded reads and cancellation; benchmark before changing.
5. **Use existing lifecycle mechanisms.** Native LSP pooling, optional inference
   admission/leases/unload, supervised child generations and bounded caches already
   exist. Establish production wiring before describing an idle-reaper method as
   operational. There is no evidence justifying a second global resource manager.

These are ranked investigations and small follow-up slices, **not measured claims
that they dominate all memory use**. The independent chapters distinguish source
facts, executed measurements, inference, hypotheses and unavailable evidence.

## Important corrections to common assumptions

- OMP2 already uses PyO3 **and** lower-level CPython APIs. Replacing its integration
  with “PyO3” is not an architectural proposal. Its embedded free-threaded 3.14
  build is not interchangeable with arbitrary abi3 interpreters. New upstream
  abi3t work for 3.15 does not retroactively change that deployed build contract.
- `CodeIR`, `CodeQueryIR`, `ChangeIR` and `VerificationIR` are not established
  production types in the scoped search. Concrete query, execution, verification
  snapshot/policy/evidence and AST types must be used by their actual names.
- Prost is already appropriate for established control protocols. Extension-host
  control also uses framed JSON; Eval uses NDJSON. There is no universal-Protobuf
  premise and no proposal to rewrite these contracts.
- Source includes legitimate trusted system C-library loading and an optional
  local inference engine. “No arbitrary native plugins” does not mean “no FFI.”
- Saetta is model/platform-specific and its normal advertised pack requirements
  exceed the 16 GiB target. Its fit guards must not be bypassed. Remote Bedrock
  operation benefits from client/context efficiency, not from access to model KV.
- `alphastorm/fast-abi` is Ethereum ABI tooling, not a native Rust ABI solution.
  xabi and emerging Rust ABI initiatives are not a demonstrated OMP2 need.
- Upstream license declarations need source verification: vectX has conflicting
  root/dependency declarations; PeerClaw/xabi have incomplete license evidence.
  Recommendations do not authorize copying those implementations.
- The Meta-Harness paper's official artifact is distinct from `s0-cli`'s independent
  adaptation. Candidate-source preflight and import warnings are not OS isolation.

## What was actually measured

The [benchmark chapter](06-benchmarks-and-memory.md) and its raw files are the
authority. The executed scope includes real workspace/dependency inventory,
focused native core/journal tests, actual native blob API probes and explicitly
synthetic ownership/scaling probes.
Synthetic `Vec` graphs are not OMP2 CodeIR, and `Arc` aliasing is not equivalent
to obtaining an independently mutable graph. No Mac physical-footprint, paid
Bedrock, model-quality, whole-harness startup or end-to-end speedup is invented.

The current workspace has 62 members and the inspected lockfile contains 1,397
package records and 155 duplicate-name/version groups. Those counts are audit
inputs, not proof that dependencies are redundant or that removing them is safe.
Existing P8 workload/census, tracing, replay tests and allocation counters are
preferred measurement seams over a second observability system.

## Reading map

| Document | Responsibility |
| --- | --- |
| [Scope and evidence](00-scope-and-evidence.md) | Checkout, authority, status vocabulary, provenance |
| [Native baseline/lifecycle](01-native-baseline-and-lifecycle.md) | Production composition, resources, registries and ADR constraints |
| [Saetta/native inference](02-saetta-and-native-inference.md) | Source comparisons, model safeguards, portability and licenses |
| [ABI/Python/data](03-abi-python-and-data-boundaries.md) | ABI candidates, CPython, IPC, CAS and data ownership |
| [Providers/context/allocation](04-providers-context-and-allocation.md) | Bedrock, streaming, context, allocation and TUI |
| [Technologies/Meta-Harness](05-technology-and-meta-harness.md) | Formats, profiling, isolation, complementary sources and legal optimization |
| [Benchmarks/memory](06-benchmarks-and-memory.md) | Executed experiments, raw commands, future platform/corpus methodology |
| [Roadmap and decisions](07-roadmap-and-decisions.md) | Priorities, dependency DAG, acceptance and rollback |
| [Fifty explicit answers](08-final-answers.md) | Answers to the owner's numbered research questions |
| [Target architecture/platform](09-target-and-platform.md) | Current/proposed diagrams, M1 feasibility and hard constraints |
| [Validation/open questions](10-validation-and-open-questions.md) | Evidence checks, deliverable coverage and unresolved experiments |
| [Decision records](adoption-records.json) | Machine-readable, fully expanded opportunity records |
| [Repository inventory](references/repositories.json) | Canonical lookup status, exact discovery SHAs and license metadata |
| [Workspace graph](references/workspace-graph.json) | All 62 packages and 344 declared internal edges with feature/target conditions |

## Retain, implement first, defer, reject

**Retain:** native calls/traits, actual cheap `Arc`/`Str` clones, existing Prost and
other versioned protocols, embedded killable CPython, pooling already present,
provider-specific semantics, journal authority and hard verification gates.

**First:** measure the real workload, then bounded artifact reads, bounded
diagnostic retention, one-render-per-request prompt projection and measured body
ownership transfer. Each is reviewable without a new runtime or mandatory server.

**Defer:** eviction changes until production resource ownership and warm/cold
costs are measured; rkyv/FlatBuffers/shared memory until selective reads, existing
CAS and streaming are insufficient; specialized local inference until a model
passes quality and concurrent-memory acceptance on the real 16 GiB machine.

**Reject for this roadmap:** unsupported Rust plugin ABIs, arbitrary third-party
in-process native extensions, wholesale protocol replacement, another capability
registry/scheduler/blob store, Pi compatibility restoration, unbounded live
self-modification, disabling security for benchmark wins, a global allocator
replacement without evidence, and making Docker/Podman/local inference mandatory.

Accepted ADRs remain unchanged. One existing external-interpreter configuration
path appears in tension with ADR 0036's embedded-only requirement; the report
records the discrepancy and proposes a conformance investigation, not an ADR
rewrite or silent removal of an existing configuration contract.
