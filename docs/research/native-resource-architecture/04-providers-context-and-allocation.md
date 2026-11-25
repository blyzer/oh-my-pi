# Providers, context projection, streaming and presentation

Scope: production inference/context/TUI source audit; remote-first optimization.
OMP2 pin: `blyzer/oh-my-pi`, `omp2@fda1315052b8b4074f9ea5acc842b2cc0b08b2b2`.
Research date: 2026-11-24. Classification **d**: targeted internal improvements,
with substantial existing functionality retained. No production change or paid
inference was executed. Every source reference below belongs to this pin.

## 1. Findings and evidence standard

**Source-verified** means the named implementation and its callers were inspected;
it does not mean a live provider request, native terminal experiment or memory
profile ran here. **Hypothesis** means a plausible cost requiring measurement.
No claim below establishes a measured performance regression or improvement.

The architecture already supplies pooled native HTTP, typed provider operations,
bounded framing, cancellation, retry evidence, prompt-cache intent, stable prompt
bands, immutable message payload sharing and a retained differential renderer.
It would be counterproductive to replace those with another client stack,
session authority or string-based terminal renderer. The best early opportunities
are eliminating repeated prompt-band work, examining long-lived transport capture
retention, and avoiding a demonstrable body-copy stage on SigV4 requests.

## 2. Applicable accepted decisions

| ADR | Exact constraint excerpt and rationale | Consequence for this research |
| --- | --- | --- |
| [0016](../../adr/0016-semantic-requests-cross-layers.md):21-31 | “The control plane MUST state semantic intent only”; inference owns capability, cost and escalation translation | Cache retention and backend strategy stay in existing request/catalog/layer contracts; no Bedrock fields in Directors |
| [0017](../../adr/0017-compatibility-as-structured-knowledge.md):42-79 | Compatibility knowledge is structured and resolves specificity deterministically; equally specific conflicting rules are errors | Provider capability updates belong to catalog compilation, not model-name branching in the agent |
| [0018](../../adr/0018-provider-is-more-than-stream.md):36-52 | “A provider is a shared piece of inference infrastructure with a typed operation surface”; shared auth/retry/rate machinery is engine-owned | Preserve `omp-ai` ownership, typed unsupported outcomes and shared transport; gateways do not justify a second provider runtime |
| [0019](../../adr/0019-forced-tool-call-escalation.md):31-46 | “Correctness beats the cache once persuasion has failed.” | No optimization may suppress forced-call escalation or falsify its receipts to improve cache metrics |
| [0030](../../adr/0030-one-pass-rendering-pipeline.md):50-69 | Push sinks, single ANSI decomposition/final materialization and pooled span buffers | Retain native `RichSink`/renderer architecture; do not copy another project's renderer based on executable-size claims |
| [0036](../../adr/0036-python-for-extensions.md):36-67 | Embedded free-threaded CPython; extension/Eval execution is in killable children, never engine-thread extension execution | Context hook optimization must preserve the external extension contract and rejection/rollback semantics; no native plugin workaround |

These source-derived architectural principles remain constraints regardless of
their historical Pi rationale. Existing tests whose names contain `matches_pi`
are evidence about the checked-in test's assertion, not a recommendation to
retain Pi compatibility as a correctness target. New fixtures should name OMP2
contracts, previous OMP2 revisions and provider wire specifications.

## 3. Implemented provider and resource inventory

| Capability / maturity | Production implementation | Ownership / lifecycle / current limitation |
| --- | --- | --- |
| Typed operations — source-verified | `crates/ai/src/client.rs:38-99,127-235`; `call.rs:1013-1046` | Typed `Client<S,P>` calls/plans flow through existing layers. Chat is not the whole provider surface. Plan and message payloads use `Arc`; do not call every clone a deep copy |
| Route/model facts — source-verified | `crates/catalog/src/compile.rs:103-108,590-596,5912-5913`; `crates/ai/src/provider/builtin.rs:960-994` | Compiled transport/capability facts choose native codecs. Catalog rows are not proof of account entitlement, model availability in a Region or a successful live request |
| HTTP pooling — source-verified | `crates/ai/src/transport/http.rs:312-337,365-379,458-465`; `crates/driver/src/registry.rs:1568-1586` | A process-wide `LazyLock` owns a pooled Hyper/rustls HTTP/1.1+HTTP/2 client. Constructing `HttpTransport::new` first accesses it; clones share the client and capture collection. No evidence of one new pool per chat turn; idle socket behavior needs measurement rather than a second pool |
| Startup preconnect — source-verified API | `crates/ai/src/transport/http.rs:322-334,475-508` | Best-effort, credential-free DNS/TCP/TLS/HTTP warmup uses the same pool. Preserve remote-first operation; compare startup gains against unnecessary connections for unused endpoints |
| Bedrock ConverseStream — source-verified | `crates/ai/src/codec/bedrock.rs:87-186,495-658` | Typed codec builds JSON and `/model/<id>/converse-stream`; default bounds are 16 MiB request/frame and 128 MiB aggregate response. Guardrail/request metadata are typed. Structured-output and service-tier intent are explicitly rejected at lines515-528 in this implementation |
| Anthropic Bedrock stream codec — implemented, routing qualification | `crates/ai/src/codec/anthropic.rs:41,1243-1259,2366`; `crates/catalog/src/compile.rs:5912-5913` | Bedrock adapter emits body `anthropic_version` and `/invoke-with-response-stream`; catalog compilation recognizes that codec. The inspected `builtin.rs` codec match has Converse but no `anthropic-bedrock` arm: source implementation is not proof a current production route can select it. Verify reachability before promising a second usable Bedrock API |
| AWS EventStream — source-verified | `crates/ai/src/transport/eventstream.rs:91-145,270-299`; Bedrock codec line180 | Bounded incremental `BytesMut` framing and CRC validation, not SSE. Cancellation clears logical buffered bytes; `clear()` is not proof that allocated capacity or physical pages were returned before decoder drop |
| AWS credentials — source-verified | `crates/ai/src/auth/aws.rs:45-52,67-72,480-545`; `auth/sigv4.rs:75-112`; `auth/lease.rs:570-609` | Scoped caching/invalidation and shared in-flight resolution already exist. Discovery distinguishes availability from secret leases. SigV4 signs the finalized method/URI/headers/body. Avoid a replacement credential resolver or logging key material |
| Credential alternatives — source-verified | `crates/ai/src/auth/broker.rs:909-926,1107-1134`; `crates/catalog/src/compile.rs:6748-6794` | Bedrock Converse can prefer an explicit bearer token; SigV4 and the distinct OpenAI-shaped Bedrock Mantle route must not be conflated. Preserve route-specific authentication semantics |
| Retry/cancellation/backpressure — source-verified | `crates/ai/src/layer/retry.rs:43-85,124-190`; `transport/http.rs:1718-1804,2044-2061` | Full-jitter capped retry is conditioned on uncommitted/replay-safe evidence; streaming polling is demand-driven with cancellation/deadline selection and cancel-on-drop. Decoder output batches can still allocate per input frame; this is not proof of end-to-end allocation-free streaming |
| Prompt cache policy — source-verified | `crates/ai/src/settings.rs:560-571`; `codec/bedrock.rs:559-578,931-932,2053-2074` | `Auto` prefers short caching for Anthropic/Converse; explicit checkpoints/retention are lowered by codecs. Read/write token counts enter usage. Hit rate, minimum-token behavior and account/Region support are not established by setting a preference |
| Immutable conversation/delta surface — source-verified | `crates/ai/src/session/store.rs:27-85`; `crates/ai/src/session/revision.rs` | Existing create/fork/revision/delta/provider-state binding contracts can support reuse. They do not imply every route sends only a delta or that Bedrock owns durable server-side conversation state |
| Stable request/roster — source-verified | `crates/agent/src/loop.rs:2404-2418,2517-2575`; `crates/ai/src/call.rs:581-588,1013-1045` | System prefix precedes durable history; volatile recall/status trails it. Wire tool declarations are latched and shared. Message contents, tools and requests use immutable `Arc` slices |
| Context patch hook — source-verified | `crates/agent/src/context.rs:87-140,218-234`; `loop.rs:2324-2363` | No subscribed hook means no view construction. The view is body-free; only a working projection is edited. Copy-on-patch preserves rejection safety and journal independence |
| Compaction — source-verified | `crates/agent/src/directors/compaction.rs:209-268,358-383` | Configurable threshold/reserve/recent-tail logic and dead-end guard already exist; it explicitly preserves stable system prefix for summarizer cache reuse. Do not introduce another compactor |
| Raw provider diagnostic ring — source-verified | `crates/ai/src/transport/capture.rs:16-26,98-159,199-219`; `transport/http.rs:1928-1943` | Intentionally always-on, redacting, bounded ring: defaults512 frames ×64 KiB maximum payload, plus bounded subscriber backlogs. 32 MiB is a payload-limit product, not measured RSS or an allocation reservation. SSE formatting/redaction can allocate and parse JSON even with no subscriber |
| Native TUI — source-verified | `crates/tui/src/renderer.rs:321-342,465-479,578-600,752,879-900`; `rich.rs`, `slots.rs` | Previous frame/layers and reusable paint/output buffers are retained; damaged presentation exists. `PaintStats` and a 64 MiB output-backlog guard are present (`renderer.rs:46-76`). Native renderer is already beyond a simple whole-screen repaint design |

### Bedrock API coverage is not a blanket implementation claim

The actual audited path is `ChatRequest → EncodeLayer → BedrockConverseCodec →
BodySource::Bytes → credential application → pooled HTTP → AWS EventStream →
typed semantic events/usage`. See `layer/encode.rs:40-53,130-139`,
`codec/bedrock.rs:140-186`, `transport/http.rs:677-698,1718-1804`.
Converse (non-streaming), InvokeModel (non-streaming) and every newly documented
Bedrock option are **not** inferred implemented merely because related codecs,
catalog enums or AWS documentation exist. No live AWS operation ran here.

Official mutable service documentation checked on 2026-11-24:

- [ConverseStream](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html)
  documents model/inference-profile selection, additional provider parameters,
  Guardrails and the streaming endpoint; its current schema includes output and
  service-tier controls that the inspected codec rejects. This is an API-coverage
  investigation candidate, not permission to silently downgrade required intents.
- [InvokeModelWithResponseStream](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_InvokeModelWithResponseStream.html)
  documents the model-specific JSON request API and its separate endpoint.
- [Prompt caching](https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html)
  supports model/API-dependent implicit and explicit strategies. Stable prefixes
  improve eligibility, not guaranteed hits. Thresholds/TTL vary by model;
  cross-Region routing can change cache writes. Evaluate actual response usage,
  not assumed savings. No live Region/account/model-availability assertion is made.

Remote prompt caching does not expose weights, tensors or local KV-cache memory.
For the M1 Pro this avoids model-resident memory while retaining client-side
projection, JSON, transport buffers and child-process costs. Cancellation can stop
local consumption, but is not a guarantee that AWS stopped billing immediately.

OmniRoute/CLIProxyAPI should use existing configured route/codec/auth/discovery
contracts where their real endpoint supports them. OMP2 contains CLIProxyAPI
import/config evidence (`crates/app/src/cli.rs:2481`,
`crates/driver/src/v1_import/models_tests.rs:55-120`) and prior configured
OmniRoute examples (`docs/research/adw-on-eval-vs-omp-adw.md:91`). These are not
live gateway compatibility proofs. Do not force Bedrock-specific reasoning,
Guardrails, cache evidence or SigV4 through a lowest-common-denominator gateway.

## 4. Allocation and retention findings

| ID | Verified code behavior | Cost inference, severity and measurement needed |
| --- | --- | --- |
| P01 | `HttpTransport.captures` is an `Arc<Mutex<Vec<HttpCaptureRecord>>>`; each response pushes a record (`transport/http.rs:365-379,807-816`); `captures()` clones/sorts all records at513-525. No drain/clear/retain path found in that file | **High-priority retention hypothesis** for a long-lived production transport assembled at `driver/registry.rs:1568-1586`. Per-attempt frame bounds do not bound total attempt records. Measure 10k/100k mock attempts, live heap and retained handles before calling it a measured leak; maintain receipts independently |
| P02 | Production `CanonicalPromptSource` (`driver/headless/kernel.rs:2981`) calls `candidate` from both `system_items/banded_render` and `volatile_items` (`agent/prompt/mod.rs:39-71`). Kernel calls both for the same DOM (`agent/loop.rs:2517-2551`) | **High-confidence repeated computation**, unknown practical magnitude: all band rendering/props may repeat within a request. First try one request-scoped prepared band result; simpler than persistent caching/new IR. Measure allocations and renderer invocations on identical prefixes |
| P03 | Converse JSON is encoded into `Bytes` (`codec/bedrock.rs:656-658`) and is a `BodySource::Bytes` (163-180). SigV4 requires buffered bytes (`auth/lease.rs:575-598`); HTTP collects `BodyReader` by `BytesMut::extend_from_slice` (`http.rs:677-698,1138-1165`) | **Source-proven copy stage**, magnitude workload-dependent. A guarded single-chunk `Bytes` transfer fast path may avoid a full body copy. Retain byte limits, cancellation, finalized-body signing and replay-consumption evidence (`body.rs:810-849`); do not cache signatures/credentials |
| P04 | Request projection reconstructs protobuf thread items/attachments (`agent/loop.rs:2542-2552`, `prompt/projection.rs:58-105`), then `Message::from_thread_items` allocates content vectors and creates owned `Str` from borrowed protobuf text (`ai/call.rs:672-694`) | **Long-context scaling hypothesis**: repeated reconstruction/data conversion, potentially blob rereads each projection. Existing revision/delta contracts are a reuse seam. Benchmark full/delta projection and attachment reuse before retaining another whole-history cache |
| P05 | Hook path copies a message vector (`agent/context.rs:117`) and mutating patches clone rows again (229-234). Message content is `Arc<[ContentPart]>`; `Str` is small-inline/O(1)-clone (`core/str.rs:1-5,30-42`) | Copies are **not wholesale text deep clones**. Vector slots/refcounts still cost O(messages); optimize only subscribed/no-op patch paths after P02/P04. Preserve atomic rejection and unchanged prompt-head semantics |
| P06 | SSE diagnostic formatting builds `String`, then always-on capture redacts/parses JSON (`transport/http.rs:1928-1943`; `capture.rs:125-132,199-219`). Main decode also creates per-frame event queues (`http.rs:1792-1797`) | **Per-token allocation hypothesis**, especially fine-grained streams. Measure payload-size/frame-rate sensitivity; retain redaction, bounded ring and subscription semantics. Prefer reusable bounded scratch/direct sanitized representation over disabling observability |
| P07 | TUI retains prior frame, images and scratch `String`s (`renderer.rs:321-342`); clears logical contents and replaces previous frame | Retention may be deliberate warm-state reuse; `clear` does not imply physical-memory release. Measure grow/shrink/idle runs, buffer capacity and rendered work before proposing trim hysteresis. No evidence justifies replacing renderer with Saetta's |

The bounded raw-provider ring and the transport's unbounded local record vector
are different stores. Likewise the authoritative journal is not either store.
Do not “fix” retention by deleting required journal evidence or conflating a
payload-free diagnostic record with the complete request/response payload.
`ExecutionBudget::default` sets staging bytes to zero (`ai/src/receipt.rs:275-285`);
`transport/cassette.rs:1007-1025` skips frame retention when that budget is zero.
P01 still concerns the per-response record itself, not a claim that every call
retains its full response. P06's raw formatting path applies to SSE; it returns
immediately for non-SSE frames and must not be charged to Bedrock EventStream
without separate evidence.

## 5. Recommended slices and acceptance

Common constraints for every P-ID: static trusted Rust inside existing crates;
Linux AMD64/ARM64 and macOS ARM64 required; remote Bedrock/Anthropic/gateways
benefit only on the applicable codec path. No extra dependency, ABI, protocol or
IR schema is required for P01–P03/P05–P07. Expected memory/latency effects are
directional hypotheses, not measured percentages. Native lifecycle/ownership and
provider security contracts remain hard gates. Preserve all cited accepted ADRs.

| Priority / slice | Smallest intended change and why | Regression / performance acceptance | Rollback / risk |
| --- | --- | --- | --- |
| P0 diagnostic baseline | Extend existing tracing/receipts/PaintStats with optional allocation scopes and per-stage timers; fixtures exercise P01–P07 | Separate projection/lowering/JSON/signing/network/first-semantic-event costs; report bytes, calls, retained captures and child memory; normal production instrumentation off or measured negligible | Disable profiling feature; never log credentials or unredacted context. No parallel metrics framework |
| P1 P02 render once | Request-scoped band preparation shared between stable/volatile extraction in `agent/prompt/mod.rs` and `loop.rs`, maintaining existing `PromptSource`/fallback semantics | Golden request bytes, band hashes, roster order, extension failure behavior unchanged; one `candidate` evaluation for the canonical path. Measure 1/10/50 MiB contexts and prompt source time/allocations | Restore old projection path; moderate trait/API integration risk, no cold persistent cache needed |
| P1 P01 bound local transport records | Add explicit bounded history/drain ownership behind `HttpTransport`'s existing diagnostics API; audit all capture consumers first | 100k deterministic attempts plateau in retained record count/live heap; captures remain ordered with explicit truncation evidence; active attempts and canonical execution receipts survive. Existing retry/cassette tests pass | Retain old collection under a temporary feature/config for comparison. Do not silently erase a consumer's promised history; API contract review required |
| P1/P2 P03 single-chunk reuse | Optimize body collection behind `BodyReader`/HTTP primitives, preserving its exact evidence bookkeeping | Byte-for-byte request/signature equivalence at fixed signing time; identical retry safety and one-shot consumption. Test empty/one/many chunks, limit overflow, cancellation, transient error, partial read; allocation/copy counts improve on pre-encoded bodies without multi-chunk regressions | Restore collect loop; moderate ownership/evidence risk, no change to AWS authentication |
| P2 P04 incremental projection | Reuse immutable DOM/conversation revisions and existing blob ownership; begin with scoped profiling rather than another global cache | Rewind/fork/compaction/tool revocation/hook changes/provider-proof changes invalidate correctly; missing/corrupt mandatory attachment still fails; warmed projection scales with changed items, bounded retained bytes | Disable optimization and rebuild from DOM; higher invalidation risk. Extend existing representation only if request-scoped reuse is insufficient |
| P2 P05/P06 reduce transient work | Move/borrow rows when valid, pool bounded event/debug scratch, maintain ring/redactor ownership | Same ContextPatch failure atomicity, Unicode/tool delta ordering, redaction and backpressure under fragmentation; lower allocations on held-out streams | Fall back to current vectors/serializer. Avoid unsafe code or a new streaming stack |
| Retain P07, investigate idle trim | Keep current renderer; optional trim only after a large transient viewport/image workload shows problematic retention | PTY resize/Unicode/grapheme/wide-cell/image/damaged-row tests and output equivalence; observe live heap/physical footprint after idle, not RSS alone | Remove trim/hysteresis if cold repaint cost or resource thrashing worsens; never drop visible/active resources |

P01 and P02 can be investigated independently. P04 should follow immutable
baseline/profiling work, not precede it. No stable native ABI or new execution
boundary is necessary for these opportunities. Optional local inference does not
remove their costs and should not displace remote-first profiling on a shared
16 GB development machine.

## 6. Reproducible verification and experiments still required

Functional commands proposed for a later implementation slice (not executed in
this source-only audit): `just test-pkg omp-ai`, `just test-pkg omp-agent`,
`just test-pkg omp-tui`. Preserve the repo nextest/doctest recipes and real PTY
requirements for TUI changes. Existing useful fixtures include:

- `crates/ai/src/transport/tests.rs:388-401,484-510,565-632,781`: cassette replay,
  cancellation, body-capture bounds and terminal paths.
- `crates/ai/src/codec/bedrock.rs:2889-3061,3711`: cache checkpoints/retention and
  CRC/truncation assertions; adapt desired behavior to OMP2/provider contracts,
  not the historical Pi names.
- `crates/agent/src/prompt/mod.rs:180-190`: embedded-template compile and same-DOM
  deterministic rendering tests; `context.rs` tests cover patch behavior.
- `crates/tui/src/renderer.rs:2632,2760,2844-2892`: resize retirement, declared
  damage and bounded writes; `crates/tui/tests/{elastic_props,seal_rebuild}.rs`.

No dedicated allocation benchmark target was found in the inspected AI/agent/TUI
manifests/file inventory; that search is not proof the entire repository lacks
profiling. Reuse the repository's broader benchmark/observability infrastructure
identified in the consolidated benchmark report.

Build deterministic fixture streams with identical semantic events split into
1-byte, token-sized, 4 KiB and 64 KiB chunks; include long tool JSON, reasoning
proofs, cache usage, service errors and cancellation between every boundary.
Feed via existing cassette transports; measure native harness stages without
network first. Add latency samples only after warmup; report samples/variance
and P50/P95/P99 only when sample size supports them. Then run an explicitly
authorized small Bedrock canary, preserving actual model/Region/auth/TTL facts
and separating transport/network/provider latency from harness time.
Large projection fixtures may exceed a provider's request budget; they are
offline scaling experiments, not permission to bypass the codec's 16 MiB default
request limit. Include explicit over-limit rejection in end-to-end fixtures.

On M1 Pro 16 GB, report parent plus child memory, physical footprint, memory
pressure/compression/swap and concurrent IDE/browser/compiler conditions. Do not
allocate a hypothetical full-device local KV cache in the normal remote scenario.
Frame/capture budgets and projection temporary memory must be included in the
same workload budget. No target memory budget or local-model capacity is claimed
measured by this audit.
