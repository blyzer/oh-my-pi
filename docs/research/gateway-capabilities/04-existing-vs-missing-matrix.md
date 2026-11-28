# OMP2 existing versus missing capability matrix

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 4. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

**How to read it.** Rows use the dimension IDs from [`evidence/01-method-and-dimension-ids.md`](evidence/01-method-and-dimension-ids.md). Status is one of Implemented, Partial, Prototype, Proposed, Unmerged, Absent, or **Not verified** (the parent did not read enough to say). Evidence is `path:line` plus symbol at the audited commit. A row marked Implemented means the code exists and is wired as the cited file shows; it does **not** mean it was exercised, tested or measured, and nothing in this table is a measurement.

**Provisional.** This is the first pass, written from what the parent read directly for deliverable 2. The per-area audits (`evidence/omp2-*.tsv`) replace every `Not verified` and `not read` entry when they land; the table is then regenerated from them. Decision classes for the gaps are assigned in the area documents (5 to 11) and collected in the [backlog](15-prioritized-architectural-backlog.md), not here.

| ID | Dimension | OMP2 status | Evidence | Note |
|---|---|---|---|---|
| G01 | Canonical provider identity | Implemented | `crates/catalog/src/lib.rs:1-5` (provider/account domains, routes, codecs, models distinct); `ProviderId`, `RouteId` imported at `crates/ai/src/router.rs:8-45` |  |
| G02 | Canonical model identity | Implemented | `crates/catalog/src/lib.rs:1-5` (opaque wire model identifiers kept apart from selectable models); `ModelKey` in `crates/ai/src/router.rs:8-45` |  |
| G03 | Model aliases | Partial | `crates/catalog/src/selection.rs` (loose selector to exact provider/model pair) | alias data and per-provider alias tables not read |
| G05 | Dynamic discovery | Partial | `crates/ai/src/discovery/` (`accounts`, `endpoints`, `probe`, `store`); `crates/catalog/src/discover.rs` | behaviour not read |
| G06-G07 | Hosted catalogs, catalog sync | Partial | `crates/catalog/data/catalog.postcard`, `crates/catalog/data/sources.lock.json`; ADR 0059 Proposed (S1, S2 landed per STATUS) | sync mechanism not read |
| G08 | Provider capability metadata | Implemented | `crates/catalog/src/capability.rs`; `OperationBits`, `CapabilityRequirement` in `crates/ai/src/router.rs:8-45` |  |
| S01 | Deterministic provider selection | Implemented | `crates/ai/src/router.rs:148` `Router` (module doc: deterministic route ranking) |  |
| S02 | Credential eligibility | Implemented | `crates/ai/src/layer/account.rs:81` `AccountPoolLayer` (doc: eligibility, cooldown, quota, rotation) | behaviour not read |
| S03 | Availability tracking | Implemented | `RouteHealth`, `Availability` in `crates/ai/src/router.rs:8-45` | how health is fed not read |
| S04 | Rate-limit awareness | Implemented | `crates/ai/src/layer/rate.rs:29` `RateLayer`; `crates/ai/src/account/rate.rs` |  |
| S05 | Session affinity | Partial | `crates/ai/src/layer/account.rs` (doc: account affinity); `crates/ai/src/session/` (`binding`, `conversation`, `revision`) | cache-affinity use not read |
| S09 | Circuit breakers | Not verified | no `circuit` symbol was searched by the parent | needs a grep in the provider audit |
| S10 | Backpressure | Implemented | `crates/ai/src/layer/admission.rs` (doc: bounded queuing, readiness backpressure) |  |
| S11 | Cancellation | Implemented | `crates/agent/src/cancel.rs`; ADR 0011 Implemented | provider-call cancel path not read |
| S12 | Retry policies | Implemented | `crates/ai/src/layer/retry.rs` (doc: pre-commit, replay-evidence-driven) |  |
| S13 | Failure classification | Implemented | `ErrorKind`, `ErrorDetail` in `crates/ai/src/error.rs` (imported at `crates/ai/src/router.rs:8-45`); `crates/ai/src/recovery/` |  |
| S14 | Quota-aware scheduling | Partial | `crates/ai/src/account/quota.rs`; `UsageReservePolicy` in `crates/ai/src/router.rs:8-45` | provider restrictions: routing must not circumvent limits |
| S06-S08 | Capability-, cost-, latency-aware routing | Partial | capability: `CapabilityRequirement`, `negotiate` in `crates/ai/src/router.rs:8-45`; cost: `PriceUnit` imported there | latency-aware ranking not found by the parent; not read |
| T01 | OpenAI Chat Completions | Implemented | `crates/ai/src/codec/openai_chat.rs` |  |
| T02 | OpenAI Responses | Implemented | `crates/ai/src/codec/openai_responses.rs` |  |
| T03 | Anthropic Messages | Implemented | `crates/ai/src/codec/anthropic.rs` |  |
| T04 | Gemini | Implemented | `crates/ai/src/codec/gemini.rs`, `crates/ai/src/codec/google_cca.rs` |  |
| T05 | Provider-specific protocols | Implemented | `crates/ai/src/codec/{bedrock,bedrock_mantle,cursor,devin,gitlab,ollama,connect}.rs` |  |
| T08 | Streaming event normalization | Implemented | `crates/ai/src/event.rs:178` `enum ChatEvent`; ADR 0022 Implemented |  |
| T12 | Canonical IR versus pairwise translators | Implemented (canonical) | `ChatEvent` and the ADR 0022 canonical turn; codecs per wire protocol under `crates/ai/src/codec/` | this is the answer to the owner's question: canonical, not pairwise |
| T06-T07, T09-T11 | Reasoning, tool-call correlation, multimodal, structured output, error normalization | Not verified | `crates/ai/src/recovery/{reasoning,thinking,tools,json}.rs` exist | details in the provider audit |
| I01-I03 | HTTP, SSE, WebSocket | Implemented | `crates/ai/src/transport/{http,sse,websocket,websocket_transport}.rs` |  |
| I05, I06, I08 | gRPC, Protobuf, bidirectional streaming | Implemented | `crates/proto/proto/omp/inference/v1/inference.proto:48` `rpc Turn(stream ...) returns (stream ...)`, `:58` `Realtime`; `crates/serve/src/inference.rs` |  |
| I07 | AWS EventStream | Implemented (transport module) | `crates/ai/src/transport/eventstream.rs`; `crates/ai/src/codec/bedrock.rs` | bedrock path not read |
| I09 | WebRTC | Implemented, default app build | `crates/ai/Cargo.toml` feature `realtime`; `crates/app/Cargo.toml:94`; `cargo tree -i webrtc` shows `omp-ai` | live voice delegation (`crates/ai/src/realtime/`) |
| I10 | HTTP/3 and QUIC | Absent from default graph | `cargo tree --offline --workspace -i quinn` errored 'did not match any packages' (the `quinn` entry exists in `Cargo.lock`) | no provider use case shown |
| I11-I12 | Async inference jobs, batch APIs | Not verified |  | needs a search in the provider audit |
| I13-I15 | Native execution, local IPC, external harness protocols | Partial | `crates/ai/src/codec/omp_native.rs`; `crates/envd/src/codex_app_server/`, `crates/envd/src/acp_client/` (ADR 0043 Partially implemented) |  |
| A01 | API keys | Implemented | `crates/ai/src/auth/key.rs`; `crates/proto/proto/omp/auth/v1/auth.proto:55` `rpc PutApiKey` |  |
| A02-A03 | OAuth, device authorization | Implemented | `crates/oauth/src/{authorization,pkce,device,token}.rs` |  |
| A04 | Official CLI delegation | Partial | ADR 0043 STATUS Partially implemented; `crates/envd/src/codex_app_server/`, `crates/envd/src/acp_client/` |  |
| A05-A06 | Credential references, OS keychain | Not verified | `crates/ai/src/auth/command.rs`, `store.rs`, `crates/secrets/src/placeholder.rs` exist | keychain use not searched |
| A07 | Refresh coordination | Partial | `crates/ai/src/account/refresh.rs`; `rpc RefreshCredential` (`auth.proto:68`) | cross-process single-flight not read |
| A08-A12 | Expiry, recovery, keepers, entitlements, region limits | Not verified |  | see authentication audit |
| X01 | Plugin mechanism | Implemented: child process, no C ABI | ADR 0036 decision 5; `cargo tree -i libloading` only via `wgpu-hal`; no `abi_stable` in `Cargo.lock` |  |
| X02 | Capability manifest | Implemented | `omp.toml` manifests (read by tests: commit `07883434cb`); `crates/driver/src/discovery/native.rs` | schema not read |
| X03-X04 | Registry, integrity, signatures | Implemented | `crates/ext/src/{index,marketplace,lock,trust}.rs`; Ed25519 at `crates/ext/src/trust.rs:1287-1299` |  |
| X05 | Permissions and isolation | Partial | `crates/ext/src/lib.rs:47` `enum TrustTier` (two tiers); ADR 0076 Proposed (middle level); ADR 0075 Partially implemented |  |
| X07-X08 | Health, upgrade and rollback | Partial | `crates/ext/src/{doctor,upgrade}.rs` | rollback not read |
| W01-W02 | Logical agents in one process, async execution | Implemented | `crates/driver/src/subagent/spawn.rs` (child-kernel composition); `crates/tools/src/task.rs` |  |
| W04-W05 | Lightweight child agents, headless workers | Partial | `crates/driver/src/subagent/workpool_runtime.rs:31` `KernelWorkpoolLauncher`; hidden same-binary child dispatch (`AGENTS.md` Architecture) | same-binary child as an agent worker not verified |
| W11 | Resource-aware worker pools | Partial | `crates/driver/src/subagent/workpool_scheduler.rs:101` `WorkpoolPolicy` (count only) | no CPU or memory input found |
| W12-W14 | Orchestrator, foreman, supervisor | Partial | `crates/agent/src/jobs.rs:699` `JobBoard`, `crates/adw/src/run.rs:173` `Run`, `crates/envd/src/child_tree.rs` | responsibility map is in deliverable 9 |
| W15-W16 | Worktree isolation, revision identity | Partial | ADR 0007 Partially implemented; `crates/kb` refuses on moved HEAD (`crates/kb/src/error.rs:31`) |  |
| L01 | Shared embedding worker | Partial | `crates/memory/src/embedding/supervisor.rs:50` `EmbeddingSupervisor` (Mnemopi only) | not a general worker |
| L02 | Tiny generative model | Partial | ADR 0023 Partially implemented; `crates/ai/src/local/tiny_catalog.rs` |  |
| L06-L07, L10 | ONNX Runtime, Candle | Prototype (opt-in features) | `crates/ai/Cargo.toml` features `local-embedding` (fastembed/ort), `local-stt`, `local-tts` (candle); not in the default graph |  |
| L08-L09, L11 | MLX, direct Core ML, llama.cpp | Absent | no `mlx`, `llama`, `coreml` crate in `Cargo.lock` (grep of package names); `local-applefm` is Apple Foundation Models | llama.cpp conflicts with a locked rule |
| C01 | LSP | Implemented | `crates/tools/src/lsp/` |  |
| C02 | SCIP import | Implemented | `crates/kb/src/producer.rs`, `crates/kb/src/graph.rs:408` `read_scip` |  |
| C03 | SCIP export | Absent (not found) | no SCIP writer found by the parent | confirm in the code-intelligence audit |
| C05-C06 | Tree-sitter, ast-grep | Implemented | `crates/ast`, `crates/tools/src/ast_grep.rs` |  |
| C08 | Tantivy | Absent | no `tantivy` entry in `Cargo.lock` |  |
| C04, C14-C17 | Glean, CodeQueryIR, CodeIR, ChangeIR, VerificationIR | Not verified |  | existence of the IRs is the first question of the code-intelligence audit |
| O01-O08 | Observability | Not verified | `crates/observability` exists (8,746 lines) | metrics, TTFT and cost accounting not read |

## What the table already shows

1. The provider runtime is gateway-class. Identity, capability negotiation, account pooling with cooldown and quota, rate reservation, admission with bounded queuing, retry, recovery and observation are layers of one fixed Tower stack (`crates/ai/src/layer/stack.rs:261-274`). Most "gateway capabilities" in the owner's section 5 therefore map to **Already implemented: reuse** or **Partially implemented: extend**, and the research question becomes where the stack is thin, not what to bolt on.
2. Protocol translation already uses a canonical representation (`ChatEvent`, ADR 0022), not pairwise translators.
3. Extensions are child-process, message-based and signed; no C ABI plugin interface exists.
4. SCIP import exists; SCIP export, Tantivy and the owner's four IR names were not found by the parent.
5. The swarm is built from child kernels, a workpool and a job board; the pool limit is a count.
6. Gaps flagged for the area audits: circuit breakers (S09), latency-aware routing (S08), async and batch inference (I11, I12), credential recovery and entitlements (A08 to A12), observability depth (O01 to O08), SCIP export (C03).

## Not verified

Every `Not verified` row above, and every "not read" note. No performance, size or memory figure appears in this document.

Back to the [index](README.md).
