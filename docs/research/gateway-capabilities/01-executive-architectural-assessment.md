# Executive architectural assessment

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 1. Audited commit `85d219ae7f` (`origin/omp2`, 2026-11-27). Research proposal: nothing here is implemented, no existing ADR was edited, nothing was pushed.

## 1. The answer in one paragraph

OMP2 already has a gateway-class native provider runtime, so the question is not which gateway features to bolt on but where its existing seams are inert, duplicated or unmeasured. The audit of 39 named reference repositories (37 readable) found few capabilities OMP2 lacks outright and several it has but never feeds: the router, account pool and quota types exist, and production does not supply their inputs. The recommended work is a short list of producers, keys and fixes on existing paths, a set of measurements that decide whether any shared-infrastructure work is worth doing, and a short list of decisions only the owner can make. No second provider runtime, scheduler, credential manager, index service, orchestration layer or control plane is justified by the evidence. This agrees with the owner's principle (section 18) and with ADR 0049's rule that a capability lives in the lowest tier that satisfies it.

## 2. What was established, and how sure we are

**Verified by reading code at `85d219ae7f`** ([deliverable 2](02-verified-current-state.md), eight area audits in `evidence/omp2-*.md`, 154 audited dimension rows in [deliverable 4](04-existing-vs-missing-matrix.md)). A mechanical checker confirmed that every cited `path:line` exists and that cited symbols occur in the file (1,000+ OMP2 citations and about 100 reference citations at the last full run); it does not confirm that a claim is true. A separate read-only spot check of 32 randomly sampled cards found 27 supported and 5 partly supported (precision errors, all corrected; none reversed a conclusion); the other 71 cards were not individually re-read ([`evidence/review-sample.md`](evidence/review-sample.md)).

**Measured** (four files, command and raw output stored; [deliverable 13](13-performance-and-benchmark-plan.md)): a count of the embedded catalog (125 providers, 171 routes, 5,289 models, 230 aliases); the size of the vendored interpreter trees (182 MB release, 166 MB debug); `python3 -c pass` startup (median 49.4 ms homebrew, 27.3 ms vendored 3.14t, 30 runs); a JSON-lines pipe round trip to a persistent child (median 14 to 17 us, 1,000 calls). All were taken on a busy shared host (load averages 7 to 24 on 10 cores) and none is a true cold start.

**Not measured**: everything else. There is no built `omp` baseline binary on this machine, so binary size, cold and warm start, idle RSS, per-child cost, IPC throughput, embedding throughput, cache effectiveness and index query latency are all plans, not results.

## 3. Verdict by area

Counts are of capability cards, each carrying exactly one of the owner's nine decision classes ([deliverable 15](15-prioritized-architectural-backlog.md) lists them all).

| Area | Cards | Reuse | Extend | Propose | Optional ext. | External | Benchmark | ADR | Unnecessary | Rejected |
|---|---|---|---|---|---|---|---|---|---|---|
| Provider runtime (5, 6) | 17 | 1 | 6 | 1 | 2 | 0 | 3 | 0 | 3 | 1 |
| Authentication (5b) | 13 | 0 | 7 | 1 | 0 | 0 | 1 | 3 | 0 | 1 |
| Extensions (7) | 14 | 3 | 4 | 0 | 0 | 1 | 1 | 3 | 0 | 2 |
| Python runtime (8) | 10 | 2 | 1 | 0 | 0 | 1 | 2 | 3 | 1 | 0 |
| IPC and serialization (6b) | 9 | 1 | 2 | 0 | 1 | 0 | 2 | 1 | 2 | 0 |
| Swarm (9) | 14 | 2 | 4 | 0 | 0 | 1 | 3 | 0 | 3 | 1 |
| Local inference (10) | 12 | 0 | 3 | 2 | 1 | 1 | 3 | 2 | 0 | 0 |
| Code intelligence (11) | 14 | 3 | 2 | 2 | 0 | 1 | 2 | 0 | 4 | 0 |
| **Total** | **103** | **12** | **29** | **6** | **4** | **5** | **17** | **12** | **13** | **5** |

| Area | Verdict | Deliverable |
|---|---|---|
| Provider runtime | Gateway-class already; the work is feeding router evidence, reading rate and quota headers, a breaker state, unit-aware cost and cache-aware affinity. Canonical turn stays; pairwise translators are not needed. | [5](05-provider-integration-recommendations.md), [6](06-transport-and-protocol-recommendations.md) |
| Authentication | Mature module set; two correctness gaps (inert cross-process refresh fence, per-child credential stacks) and one policy gap (reading another application's stored token). | [5b](05b-authentication-and-credential-lifecycle.md) |
| Extensions | Already message-based, signed and child-process; manifest codes are declared but never produced. Native shared-library plugins rejected; Wasm and native-executable runtimes need an ADR 0036 decision. | [7](07-abi-independent-extension-assessment.md) |
| Python runtime | Embedded CPython remains justified; no evidence supports changing ADR 0036. | [8](08-python-runtime-comparative-assessment.md) |
| IPC and serialization | Keep Protobuf and the existing framing; fix three hygiene issues; no measured case for Cap'n Proto, FlatBuffers, rkyv or MessagePack. | [6b](06b-ipc-and-serialization.md) |
| Swarm | One root session with in-process child kernels, a workpool and a job board already exists; no new orchestrator, foreman or supervisor component is justified. | [9](09-single-session-swarm-assessment.md) |
| Local inference | Mostly opt-in or prototype; the dividing line is the process boundary; one memory budget is the missing piece. | [10](10-local-inference-assessment.md) |
| Code intelligence | SCIP import exists; no exporter, Glean or Tantivy; the four IR names are not in code. | [11](11-code-intelligence-and-scip-assessment.md) |

## 4. The ten most important findings

1. **The provider runtime is already gateway-class.** One fixed Tower stack per route (AccountPool, Admission, TransportRetry, Rate, Recovery and more, `crates/ai/src/layer/stack.rs:261-274`), a deterministic capability-first router (`crates/ai/src/router.rs:148`), a canonical turn (`ChatEvent`, `crates/ai/src/event.rs:178`) and a gRPC gateway surface (`crates/proto/proto/omp/gateway/v1/gateway.proto:21` Hello, `:29` ForwardProxy; `Inference`, `Auth`, `Blob`).
2. **Its adaptive inputs are inert in production.** Production `Router::new` supplies no runtime evidence (`crates/ai/src/router.rs:785`), `observe_rate` and `observe_quota` have no non-test callers (`crates/ai/src/account/pool.rs:750`, `:769`), no circuit breaker exists, the hosted-catalog overlay is a prototype, and there is no batch API (PRV-01 to PRV-05).
3. **Credential refresh is weaker than its ADR says.** The cross-process lease fence is inert because the owner is the constant `"omp-auth-refresh"` (`crates/driver/src/registry.rs:1381`, `crates/ai/src/auth/store.rs:1196`); each in-process child builds its own credential stack (`crates/driver/src/subagent/spawn.rs:726-738`); the two refresh paths disagree on disabling an account; and a leader crash can replay a spent refresh token (AUTH-01, AUTH-03, AUTH-07, AUTH-11). The effects on rotating tokens are inferred, not tested.
4. **The swarm needs no new layer.** Subagents are child kernels (`crates/driver/src/subagent/spawn.rs`), a persistent workpool schedules them (`workpool_scheduler.rs:101`), the job board is a supervision index, ADW is a pure state machine, and envd supervises processes. The real gaps are small: the pool limit is a count with no CPU or memory input, `SV_TASK_MAX_RUNTIME` defaults to `Never`, no auto-restart of a failed child, and no liveness check for orphaned worktrees (SWM-05, SWM-06, SWM-13).
5. **Extensions are already ABI-independent.** Child processes with a JSON CONTROL channel and Protobuf DATA, Ed25519-signed index (`crates/ext/src/trust.rs`), no C ABI plugin interface; `libloading` appears only through `wgpu-hal`. Several manifest codes (`EVendorNative`, `EAbiExport`, `ECapUnknown` and others) are declared but have no producer, and the Rust side ignores `omp_api` (EXT-03 to EXT-05). The "existing Go executable adapters" named in the brief do not exist on this commit.
6. **Embedded CPython is not what isolates extensions; the child process is.** Embedding buys no host-Python dependency, the native `_omp` module, one shippable binary and free-threaded warm workers. A standalone-CPython Eval mode already exists (`OMP_PYTHON_INTERPRETER`, `crates/envd/src/eval/process.rs:74-148`) and is absent from ADR 0036's text. No evidence supports replacing ADR 0036 (PY-01, PY-10).
7. **SCIP import exists; the rest of the code-intelligence vision is mostly absent or parked.** `omp-kb` merges SCIP producers (`crates/kb/src/producer.rs`) with tree-sitter and refuses to build if HEAD moves; only `omp kb` calls it; its cache is per Git worktree, so a child view without `.git` cannot use it. `CodeQueryIR`, `CodeIR`, `ChangeIR` and `VerificationIR` appear only in parked notes. Glean and Tantivy are unnecessary for now (CIX-03, CIX-08, CIX-09, CIX-11, CIX-12).
8. **Local inference is mostly opt-in or prototype, and unbounded in total.** The Mnemopi embedding worker exists but nothing builds it; memory pools are per consumer; `TinyDevice` is dead. llama.cpp already works as an external local server; linking it in process contradicts the locked candle-only rule; the opt-in `fastembed` path already downloads ONNX Runtime (INF-01 to INF-10).
9. **Keep Protobuf; fix wire hygiene; do not adopt zero-copy formats on theory.** `Blob.Put` buffers up to 64 MiB before hashing, envd reads allocate the declared frame length first, the extension CONTROL channel is JSON while `toolhost.proto` describes Protobuf, and the research file on serialization is stale (`SCHEMA_REV` 27 against 30 in code). Cap'n Proto, FlatBuffers, MessagePack and shared memory are absent, with no measured case for them (IPC-01 to IPC-09).
10. **The references are mostly cautionary on terms, licence and scope.** Their useful ideas are few and small (quota from response headers, a per-connection breaker, per-provider refresh lead, version negotiation and a health probe). Many patterns are built around usage limits, client identity or shared subscriptions and are rejected. Four repositories are AGPL-3.0 or GPL-3.0 (study only), three have no licence, and two named repositories (`microsoft/onnxruntime`, `AmethystDev-Labs/QuickCLI`) are unavailable (PRV-09, EXT-09, EXT-14, SWM-14).

## 5. What to do, and in what order

[Deliverable 17](17-incremental-implementation-roadmap.md) sequences six slices: baseline and decisions; credential and routing-evidence correctness; hardening of existing contracts; sharing instead of duplicating; measured provider efficiency; optional external integrations. Four draft ADRs (0077 to 0080) are provided where a decision record is justified ([deliverable 16](16-draft-adrs.md)). Nothing is scheduled that needs an ADR decision first.

## 6. What not to do

Do not add a second provider runtime, scheduler, credential manager, memory system, config store or index service. Do not add pairwise translators, a mandatory dashboard or database server, native shared-library plugins, or a Foreman or swarm crate. Do not adopt a zero-copy format, Glean or Tantivy without a measured case. Do not use routing, pooling or header observation to get around a provider's usage limits ([deliverable 14](14-security-and-licensing-assessment.md)). Do not replace ADR 0036 on present evidence.

## 7. Decisions needed from the owner

[Deliverable 20](20-open-questions.md) lists 19 questions. The ones that block the most work: whether the Anthropic server-side advisor tool (ADR 0074) overrides the locked "no vendor server-side tools" rule; whether to wire the router evidence feed; whether to accept per-process credential ownership and a shared auth stack for children; what to do with the existing CLIProxyAPI credential import and the unbound extension credential domain; the process-boundary rule for local ML runtimes; whether `omp-serve` is meant for third-party clients; and the performance thresholds that make any benchmark pass or fail.

## 8. Limits of this assessment

- Reference repositories were read at different depths (stated per repository); large repositories were read selectively and several matrix cells are `U` or `D`. `QuantumNous/new-api` was read last and shallowly (56 of its 95 matrix rows are `U`); `fallencolors/freellmapi` more fully.
- ONNX Runtime was assessed from documentation only; QuickCLI not at all.
- The Mermaid diagrams were not rendered.
- Reading was shared between several agents working in parallel; each area document names what it did not examine, and apart from the 32-card sample just described, no area audit was independently reviewed by a second reader.
- Behavioural claims (for example double refresh of a rotating token) are inferences from code, not test results.

Back to the [index](README.md).
