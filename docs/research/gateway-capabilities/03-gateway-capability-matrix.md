
## 4. Capability matrix

Legend for repository columns: `Y` implemented in code (code read), `P` partial or limited, `N` searched and not present, `D` documented only (README or docs claim, code not read), `U` not examined, `.` no row written for that dimension. The **OMP2** column comes from the OMP2 audits ([deliverable 4](04-existing-vs-missing-matrix.md)) on commit `85d219ae7f`: `Y` Implemented, `P` Partial, `R` Prototype (unwired, test-only or opt-in), `A` Proposed (ADR text only), `N` Absent, `.` not audited. Dimension IDs are defined in [`evidence/01-method-and-dimension-ids.md`](evidence/01-method-and-dimension-ids.md).

### 4.1 Inference gateways

Rows: registry, authentication, scheduling, translation, interfaces, efficiency, observability. Sources: [group A notes](evidence/ref-gateways-a.md) (CLIProxyAPI, CLIProxyAPIPlus, OmniRoute; a single provider-runtime agent wrote 62 rows each, so authentication rows are mostly `.`) and [group B notes](evidence/ref-gateways-b.md).

| ID | Dimension |  OMP2 |CLIProxyAPI | CLIProxyAPIPlus | OmniRoute | 9router | freellmapi | llmhub | new-api | ProxyCast |
|---|---|---|---|---|---|---|---|---|---|---|
| A01 | API keys | Y | . | . | . | P | Y | Y | Y | Y |
| A02 | OAuth | Y | . | . | . | Y | N | Y | P | Y |
| A03 | device authorization | Y | . | . | . | U | N | Y | U | Y |
| A04 | official CLI delegation | P | . | . | . | U | N | N | U | P |
| A05 | credential references (secret never inline) | Y | . | . | . | N | P | N | U | P |
| A06 | OS keychain | P | . | . | . | N | N | N | U | N |
| A07 | refresh coordination | P | . | . | . | U | N | Y | P | P |
| A08 | credential expiration handling | Y | . | . | . | D | P | Y | P | Y |
| A09 | credential recovery | P | . | . | . | U | P | Y | U | U |
| A10 | independent credential keepers | P | . | . | . | U | N | Y | U | U |
| A11 | authorized subscription entitlements | P | . | . | . | U | N | P | P | P |
| A12 | account and regional restrictions | P | . | . | . | U | P | P | U | U |
| E01 | prompt-cache awareness | Y | U | U | P | P | P | P | U | U |
| E02 | session affinity for cache reuse | P | U | U | U | U | P | Y | P | U |
| E03 | incremental response chaining | Y | U | U | U | U | N | P | U | U |
| E04 | provider-side state | Y | U | U | U | U | N | P | U | U |
| E05 | context retransmission handling | Y | U | U | P | U | P | P | U | U |
| E06 | semantic compression | P | U | U | P | Y | P | N | U | U |
| E07 | batch inference | N | U | U | P | U | N | Y | U | U |
| E08 | connection pooling | Y | U | U | U | U | U | U | U | P |
| E09 | streaming backpressure | P | U | U | U | U | U | U | U | U |
| E10 | token accounting | Y | U | U | U | P | P | Y | P | Y |
| E11 | cache accounting | Y | U | U | U | U | P | Y | U | U |
| E12 | time-to-first-token and latency measurement | Y | U | U | U | U | Y | P | U | U |
| G01 | canonical provider identity | Y | P | Y | U | Y | Y | P | Y | P |
| G02 | canonical model identity | Y | U | U | U | P | P | P | U | P |
| G03 | model aliases | Y | U | U | Y | Y | Y | Y | U | Y |
| G04 | provider-specific deployments | P | U | U | U | P | P | Y | U | P |
| G05 | dynamic discovery | Y | P | U | P | U | Y | Y | U | U |
| G06 | hosted model catalogs | R | U | U | U | P | Y | U | U | U |
| G07 | catalog synchronization | P | U | U | U | P | Y | P | U | U |
| G08 | provider capability metadata | Y | P | U | U | Y | Y | Y | U | P |
| G09 | authentication requirements declared per pr... | Y | . | . | . | U | P | Y | P | Y |
| G10 | free-tier and anonymous discovery | P | . | . | . | D | Y | N | U | U |
| G11 | connection identity and deduplication | P | . | . | . | U | P | P | U | U |
| I01 | HTTP | Y | Y | U | U | Y | Y | Y | Y | Y |
| I02 | SSE | Y | P | U | U | Y | Y | Y | Y | Y |
| I03 | WebSocket | Y | Y | P | U | U | N | N | U | Y |
| I04 | persistent inference sessions | Y | P | U | U | U | N | N | U | U |
| I05 | gRPC | Y | U | U | U | U | N | N | U | U |
| I06 | Protobuf | Y | . | . | . | Y | N | N | U | U |
| I07 | AWS EventStream | Y | U | U | U | Y | N | N | U | Y |
| I08 | bidirectional streaming | Y | P | U | U | U | N | N | U | U |
| I09 | WebRTC | Y | U | U | U | U | N | N | U | U |
| I10 | HTTP/3 and QUIC | N | U | U | U | U | N | N | U | U |
| I11 | asynchronous inference jobs | P | U | U | U | U | P | Y | Y | U |
| I12 | batch APIs | N | U | U | P | U | N | Y | U | U |
| I13 | native execution | Y | U | U | U | U | N | N | N | U |
| I14 | local IPC | Y | . | . | . | U | P | N | U | U |
| I15 | external agent-harness protocols | P | U | U | U | P | P | P | U | P |
| O01 | provider health | P | D | U | Y | U | Y | Y | P | Y |
| O02 | authentication health | P | . | . | . | U | P | Y | U | U |
| O03 | metrics (sessions, agents, tokens, cache, q... | P | U | P | P | P | P | P | U | P |
| O04 | cost accounting | Y | U | U | Y | P | P | Y | Y | U |
| O05 | structured runtime events | Y | U | U | U | U | P | Y | U | Y |
| O06 | management surface (UI or API) | . | . | . | . | Y | Y | Y | Y | Y |
| O07 | artifact reporting | . | . | . | . | U | N | P | U | P |
| O08 | deterministic execution status | . | . | . | . | N | N | N | N | N |
| S01 | deterministic provider selection | Y | Y | Y | Y | Y | Y | Y | Y | Y |
| S02 | credential eligibility | Y | . | . | . | Y | Y | Y | P | Y |
| S03 | availability tracking | P | Y | U | Y | Y | Y | Y | P | Y |
| S04 | rate-limit awareness | P | Y | U | Y | P | P | Y | U | U |
| S05 | session affinity | P | P | Y | Y | P | Y | Y | P | U |
| S06 | capability-aware routing | Y | U | U | P | Y | Y | Y | U | P |
| S07 | cost-aware routing | P | U | U | Y | D | D | Y | U | U |
| S08 | latency-aware routing | R | U | U | Y | U | Y | Y | U | U |
| S09 | circuit breakers | N | N | N | Y | Y | Y | Y | U | P |
| S10 | backpressure | Y | P | U | Y | U | P | Y | U | U |
| S11 | cancellation | Y | U | U | U | U | Y | Y | U | U |
| S12 | retry policies | Y | P | U | P | Y | Y | Y | Y | P |
| S13 | failure classification | Y | P | U | P | Y | P | Y | P | Y |
| S14 | quota-aware scheduling | P | P | U | Y | U | P | Y | U | U |
| T01 | OpenAI Chat Completions | Y | Y | Y | U | Y | Y | Y | Y | Y |
| T02 | OpenAI Responses | Y | Y | U | U | Y | Y | Y | Y | P |
| T03 | Anthropic Messages | Y | Y | U | U | Y | Y | Y | Y | Y |
| T04 | Gemini | Y | Y | U | U | Y | Y | P | Y | P |
| T05 | provider-specific protocols | Y | Y | U | U | Y | Y | Y | Y | Y |
| T06 | reasoning normalization | Y | P | U | P | U | P | Y | P | P |
| T07 | tool-call correlation | Y | U | U | U | U | P | Y | U | U |
| T08 | streaming event normalization | Y | Y | U | U | Y | Y | Y | U | Y |
| T09 | multimodal content | P | U | U | U | Y | Y | Y | Y | U |
| T10 | structured output | P | U | U | U | U | P | P | U | U |
| T11 | error normalization | Y | U | U | U | Y | Y | Y | P | U |
| T12 | translation architecture (pairwise translat... | Y | Y | Y | P | Y | Y | Y | P | Y |

### 4.2 Provider implementations

Sources: [provider notes](evidence/ref-providers.md).

| ID | Dimension |  OMP2 |kiro.rs (hank9999) | kiro.rs (BenedictKing) | kiro.rs (M-JYuan) | kiro.rs (Foxfishc) | kiro.rs (lzt404) | freetier2api plugin | perplexity plugin |
|---|---|---|---|---|---|---|---|---|---|
| A01 | API keys | Y | Y | U | U | U | Y | Y | N |
| A02 | OAuth | Y | Y | Y | Y | Y | Y | Y | N |
| A03 | device authorization | Y | N | N | N | N | N | D | N |
| A04 | official CLI delegation | P | N | N | N | N | N | N | N |
| A05 | credential references (secret never inline) | Y | P | P | P | P | P | P | D |
| A06 | OS keychain | P | N | N | N | N | N | N | N |
| A07 | refresh coordination | P | P | P | P | P | P | Y | N |
| A08 | credential expiration handling | Y | Y | Y | Y | Y | Y | P | P |
| A09 | credential recovery | P | P | P | P | P | P | P | P |
| A10 | independent credential keepers | P | N | N | P | P | P | P | N |
| A11 | authorized subscription entitlements | P | P | P | P | Y | Y | P | D |
| A12 | account and regional restrictions | P | P | U | U | Y | U | P | U |
| G09 | authentication requirements declared per pr... | Y | P | P | P | P | P | D | N |
| G10 | free-tier and anonymous discovery | P | N | N | N | N | N | Y | U |
| G11 | connection identity and deduplication | P | Y | U | U | U | U | P | D |

### 4.3 Configuration, extension management and plugin mechanisms

Source: [extension-management notes](evidence/ref-extension-mgmt.md).

| ID | Dimension |  OMP2 |EasyCLIProxyAPI | ecpa-plugins-store | quickx | go-plugin | wasmtime | wit-bindgen | CLIProxyAPI | CLIProxyAPIPlus |
|---|---|---|---|---|---|---|---|---|---|---|
| X01 | plugin mechanism (state which: native share... | Y | N | D | Y | Y | Y | Y | Y | Y |
| X02 | capability manifest | P | N | P | P | N | D | Y | P | N |
| X03 | registry and distribution | P | N | Y | Y | N | N | N | Y | N |
| X04 | artifact integrity and signatures | Y | P | P | N | P | N | N | P | N |
| X05 | permissions and isolation | P | N | N | N | P | Y | N | N | N |
| X06 | lazy startup and independent termination | Y | Y | N | N | Y | P | N | P | N |
| X07 | health reporting | P | P | N | N | Y | N | N | U | U |
| X08 | upgrade and rollback | Y | P | P | N | N | N | N | P | N |
| X09 | platform-specific executable distribution | P | P | D | N | N | D | P | Y | N |
| X10 | dependency declarations | Y | N | N | P | N | D | N | N | N |
| X11 | compatibility declarations | P | P | P | N | Y | D | D | P | N |

### 4.4 Agent harnesses

Source: [agent-harness notes](evidence/ref-agent-harness.md).

| ID | Dimension |  OMP2 |cotion | grape | lime |
|---|---|---|---|---|---|
| W01 | multiple logical agents in one process | Y | N | N | P |
| W02 | asynchronous agent execution | Y | P | Y | P |
| W03 | native worker threads | N | U | U | U |
| W04 | lightweight OMP child agents | P | U | U | U |
| W05 | headless OMP workers | A | N | P | U |
| W06 | recursive self-orchestration | Y | N | U | U |
| W07 | external agent harnesses | P | Y | D | U |
| W08 | sandboxed subprocesses | Y | N | U | P |
| W09 | shared runtime services (provider registry,... | P | N | P | U |
| W10 | lazy capability initialization | P | U | U | U |
| W11 | resource-aware worker pools | P | N | P | P |
| W12 | orchestrator responsibilities (decompositio... | P | P | Y | U |
| W13 | foreman responsibilities (workstream owners... | P | U | P | U |
| W14 | supervisor responsibilities (lifecycle, can... | P | P | P | P |
| W15 | Git worktree and copy-on-write isolation | P | N | D | P |
| W16 | workspace revision identity | P | N | U | U |

### 4.5 Local inference and code intelligence

Sources: [local-inference notes](evidence/ref-local-inference.md) and [code-intelligence notes](evidence/ref-code-intelligence.md).

| ID | Dimension |  OMP2 |llama.cpp | mlx | mlx-lm | candle | coremltools | scip | Glean | tantivy |
|---|---|---|---|---|---|---|---|---|---|---|
| C02 | SCIP import | Y | . | . | . | . | . | Y | D | . |
| C03 | SCIP export | N | . | . | . | . | . | Y | . | . |
| C04 | Glean | N | . | . | . | . | . | . | D | . |
| C05 | tree-sitter | Y | . | . | . | . | . | . | U | . |
| C07 | tgrep or native trigram search | P | . | . | . | . | . | . | . | P |
| C08 | Tantivy | N | . | . | . | . | . | . | . | Y |
| C09 | native text search | Y | . | . | . | . | . | . | . | Y |
| C10 | structural summaries | Y | . | . | . | . | . | P | . | . |
| C15 | CodeIR | A | . | . | . | . | . | N | . | . |
| C16 | ChangeIR | A | . | . | . | . | . | N | . | . |
| C18 | sharing across agents | P | . | . | . | . | . | P | D | P |
| C19 | revision and document-version consistency | P | . | . | . | . | . | P | D | P |
| C20 | incremental invalidation | P | . | . | . | . | . | D | . | Y |
| L01 | shared embedding worker | R | D | U | U | P | U | . | . | . |
| L02 | tiny generative model | P | Y | U | Y | P | U | . | . | . |
| L03 | local classifier | N | N | U | U | U | U | . | . | . |
| L04 | reranker | R | Y | U | U | U | U | . | . | . |
| L05 | semantic compression | P | N | U | U | U | U | . | . | . |
| L06 | ONNX Runtime CPU | R | N | U | U | U | U | . | . | . |
| L07 | ONNX Runtime Core ML | N | U | U | U | U | N | . | . | . |
| L08 | MLX | N | N | Y | Y | U | U | . | . | . |
| L09 | direct Core ML | N | U | U | U | U | Y | . | . | . |
| L10 | Candle | R | U | U | U | Y | U | . | . | . |
| L11 | llama | P | Y | U | U | U | U | . | . | . |
| L12 | Apple Neural Engine use | N | U | N | U | U | Y | . | . | . |
| L13 | GPU and Metal | R | D | Y | P | Y | U | . | . | . |
| L14 | shared model residency across agents | P | Y | P | Y | N | U | . | . | . |
| L15 | Linux without Docker or Podman | P | D | D | U | P | P | . | . | . |
| L16 | hardware-aware workload allocation | N | D | D | N | U | P | . | . | . |

### 4.6 Serialization and RPC libraries

Source: [serialization notes](evidence/ref-serialization.md).

| ID | Dimension |  OMP2 |prost | tonic | capnp-rust | flatbuffers | rkyv |
|---|---|---|---|---|---|---|---|
| I05 | gRPC | Y | N | Y | D | U | . |
| I06 | Protobuf | Y | Y | Y | . | . | . |
| I08 | bidirectional streaming | Y | N | Y | P | N | N |
| I14 | local IPC | Y | . | U | . | . | . |
| Z01 | JSON/JSONL | Y | N | N | . | . | . |
| Z02 | Protobuf | Y | Y | . | . | . | . |
| Z03 | Cap'n Proto | N | . | . | Y | . | . |
| Z04 | FlatBuffers | N | . | . | . | Y | . |
| Z05 | rkyv | N | . | . | . | . | Y |
| Z07 | shared memory or memory-mapped files | N | . | . | U | . | U |
| Z08 | binary tensor buffers | P | . | . | D | D | D |
| Z10 | gRPC | Y | . | Y | . | . | . |

## 5. What the matrix shows

**Where at least two inference gateways implement something that OMP2 only partly has, or lacks** (computed from the grid in 4.1: two or more `Y` among the eight gateways and OMP2 not `Y`):

| ID | Dimension | OMP2 | Gateways with `Y` |
|---|---|---|---|
| I11 | asynchronous inference jobs | P | llmhub, new-api |
| O01 | provider health | P | OmniRoute, freellmapi, llmhub, ProxyCast |
| O06 | management surface (UI or API) | . | 9router, freellmapi, llmhub, new-api, ProxyCast |
| S03 | availability tracking | P | CLIProxyAPI, OmniRoute, 9router, freellmapi, llmhub, ProxyCast |
| S04 | rate-limit awareness | P | CLIProxyAPI, OmniRoute, llmhub |
| S05 | session affinity | P | CLIProxyAPIPlus, OmniRoute, freellmapi, llmhub |
| S07 | cost-aware routing | P | OmniRoute, llmhub |
| S08 | latency-aware routing | R | OmniRoute, freellmapi, llmhub |
| S09 | circuit breakers | N | OmniRoute, 9router, freellmapi, llmhub |
| S14 | quota-aware scheduling | P | OmniRoute, llmhub |
| T09 | multimodal content | P | 9router, freellmapi, llmhub, new-api |

These line up with the provider cards PRV-01 to PRV-03, PRV-06, PRV-07 and PRV-16 ([deliverable 5](05-provider-integration-recommendations.md)): availability and health evidence, rate-limit and quota observation, session affinity for cache reuse, cost- and latency-aware ranking, circuit breakers and metrics. The OMP2 types for most of them exist; the provider audit found the production producers missing. The list is a prompt for reading the cards, not a verdict: a `Y` in a reference repository says nothing about whether the behaviour is safe or allowed (section 3 and [deliverable 14](14-security-and-licensing-assessment.md)).

**Other points the notes establish** (citations in the linked notes):

- CLIProxyAPI and CLIProxyAPIPlus translate with pairwise translators; OMP2's canonical turn needs one codec per wire protocol ([PRV-14](06-transport-and-protocol-recommendations.md)).
- CLIProxyAPI reads quota from response headers, the producer OMP2 lacks ([PRV-02](05-provider-integration-recommendations.md)); OmniRoute scopes a circuit breaker per connection ([PRV-03](05-provider-integration-recommendations.md)).
- CLIProxyAPI's plugin ABI is `dlopen` plus JSON buffers with SHA-256 integrity and no unloading; CLIProxyAPIPlus has no plugin host (static Go registration); `hashicorp/go-plugin` contributes two ideas OMP2 lacks, version negotiation across a set of API levels and a health probe ([deliverable 7](07-abi-independent-extension-assessment.md)).
- None of the seven provider-implementation repositories has an OS keychain or encrypts credentials at rest ([deliverable 5b](05b-authentication-and-credential-lifecycle.md)).
- Patterns built around usage limits, client identity or shared subscriptions are catalogued in [deliverable 14](14-security-and-licensing-assessment.md), not as capabilities to adopt.

## 6. Limits

- Depth varies by repository and is stated in the reading notes. Several rows are `U` or `D`; large repositories (OmniRoute, new-api, llama.cpp, mlx) were read selectively.
- `QuantumNous/new-api` was read last and shallowly: 56 of its 95 rows are `U` (per-vendor adaptors, stream scanner, affinity store and web UI not read); `fallencolors/freellmapi` was read more fully (only E08 and E09 are `U`). The freellmapi clone's remote is `fallencolors/freellmapi` but its README badges point to `tashfeenahmed/freellmapi`; with a single commit in the shallow clone the original could not be established. `hashicorp/go-plugin`, `wasmtime` and `wit-bindgen` are libraries, so only extension rows apply.
- A `Y` is not a quality grade, and no performance claim about any repository is made.

Back to the [index](README.md).
