# Cross-gateway capability matrix

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 3. Reference repositories pinned in [`evidence/ref-inventory.md`](evidence/ref-inventory.md). Research proposal: nothing here is implemented.

**How this document is built.** Sections 1 to 3 record what each reference repository is, its pinned commit, its licence and how much code it has, verified directly. Section 4 is assembled mechanically from the per-repository files `evidence/matrix-<owner>_<repo>.tsv`, each row of which carries an `owner/repo@sha7:path` citation; the grids below show only the status letter, so open the TSV for the evidence. Rows an agent did not examine are `.` (no row written) or `U`. The depth of reading differs by repository and is stated in each reading note; a `Y` means the agent read code that implements it, not that it works well.

## 1. Availability

Of the 39 repositories named in the owner's section 3, 37 were cloned shallowly (`git clone --depth 1 --no-tags`) and 2 were unavailable on 2026-11-28:

- `AmethystDev-Labs/QuickCLI`: `git ls-remote` printed `ERROR: Repository not found`; `gh api repos/AmethystDev-Labs/QuickCLI` returned HTTP 404; `gh api users/AmethystDev-Labs/repos` lists no repository with that name. Not assessed.
- `microsoft/onnxruntime`: same two errors. The sibling repositories `microsoft/onnxruntime-genai`, `-extensions` and `-inference-examples` also returned not found, while `microsoft/vscode` and `pykeio/ort` resolved. ONNX Runtime can therefore only be assessed from its official documentation (labelled "documentation only, code not read") in deliverable 10.

## 2. What each cloned repository is (verified facts only)

Source-file counts are the number of files with a source extension (`.go .rs .ts .tsx .js .py .java .cpp .cc .c .swift .kt .vue .mjs`) found by walking the clone, skipping `.git`, `node_modules`, `vendor` and `target`; the three most common extensions are shown. Licence is the GitHub API `license.spdx_id` plus the licence files present in the clone. The README title is the first Markdown heading. None of this says what the code does; that is the reading in `evidence/ref-*.md`.


### Inference gateways

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [QuantumNous/new-api](https://github.com/QuantumNous/new-api) | `04dfcbb` | AGPL-3.0 / LICENSE | .go 1152, .tsx 983, .ts 527 | New API |
| [Ve-ria/CLIProxyAPIPlus](https://github.com/Ve-ria/CLIProxyAPIPlus) | `60bb379` | AGPL-3.0 / LICENSE, LICENSE-MIT | .go 617, .tsx 40, .ts 4 | CI/CD |
| [cheaper-inference/OmniRoute](https://github.com/cheaper-inference/OmniRoute) | `3e23a8a` | MIT / LICENSE | .ts 15352, .tsx 1955, .mjs 951 | OmniRoute - The Free AI Gateway |
| [decolua/9router](https://github.com/decolua/9router) | `5ae63d2` | MIT / LICENSE | .js 1610, .mjs 17 | 9Router - FREE AI Router & Token Saver |
| [fallencolors/freellmapi](https://github.com/fallencolors/freellmapi) | `b5d8333` | MIT / LICENSE | .ts 612, .tsx 104, .mjs 11 | FreeLLMAPI |
| [hank9999/proxycast](https://github.com/hank9999/proxycast) | `3ad8ff6` | none detected / no licence file | .rs 203, .tsx 136, .ts 65 | ProxyCast 🚀 |
| [nicolaeser/llmhub](https://github.com/nicolaeser/llmhub) | `8bdc1de` | MIT / LICENSE | .ts 1083, .tsx 321, .mjs 54 | LLM Hub |
| [router-for-me/CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) | `dbe641f` | MIT / LICENSE | .go 2523, .c 16, .rs 15 | CLI Proxy API |

### Provider implementations

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [BenedictKing/kiro.rs](https://github.com/BenedictKing/kiro.rs) | `87a735c` | none detected / no licence file | .rs 71, .tsx 24, .ts 8 | kiro-rs |
| [Foxfishc/kiro.rs](https://github.com/Foxfishc/kiro.rs) | `15c0a3a` | MIT / LICENSE | .rs 73, .tsx 24, .ts 8 | kiro-rs |
| [M-JYuan/kiro.rs](https://github.com/M-JYuan/kiro.rs) | `fc35283` | MIT / LICENSE | .rs 70, .tsx 20, .ts 8 | kiro-rs |
| [cxy2730/CLIProxyAPI-freetier2api-plugin](https://github.com/cxy2730/CLIProxyAPI-freetier2api-plugin) | `0a718a6` | MIT / LICENSE | .go 299 | FreeTier 2API Plugin |
| [hank9999/kiro.rs](https://github.com/hank9999/kiro.rs) | `3c9723f` | MIT / LICENSE | .rs 53, .tsx 19, .ts 6 | kiro-rs |
| [lzt404/kiro.rs](https://github.com/lzt404/kiro.rs) | `8331585` | MIT / LICENSE | .rs 71, .tsx 23, .ts 8 | kiro-rs |
| [stark-shadow/cpa-plugin-perplexity](https://github.com/stark-shadow/cpa-plugin-perplexity) | `0d09161` | MIT / LICENSE | .go 15, .js 4, .py 2 | Perplexity - a CLIProxyAPI native provider plugin |

### Configuration and extension management

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [Ve-ria/quickx](https://github.com/Ve-ria/quickx) | `e13db37` | GPL-3.0 / LICENSE | .tsx 25, .ts 21, .js 1 | quickx |
| [lzt404/EasyCLIProxyAPI](https://github.com/lzt404/EasyCLIProxyAPI) | `faf29b2` | MIT / LICENSE | .ts 102, .rs 73, .tsx 49 | Overview |
| [stark-shadow/ecpa-plugins-store](https://github.com/stark-shadow/ecpa-plugins-store) | `2d82289` | none detected / no licence file | n/a | ECPA Plugins Store |

### Agent harness and orchestration

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [Ve-ria/cotion](https://github.com/Ve-ria/cotion) | `b7a4307` | MIT / LICENSE | .ts 21, .mjs 3, .tsx 1 | none |
| [grape-dev/grape](https://github.com/grape-dev/grape) | `b31f8d8` | MIT / LICENSE | .py 2520, .js 210, .vue 48 | 1. What Grape is |
| [limecloud/lime](https://github.com/limecloud/lime) | `015b878` | GPL-3.0 / LICENSE, licenses, LICENSES.md | .rs 4403, .ts 3445, .tsx 1339 | Lime |

### ABI-independent extension mechanisms

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [bytecodealliance/wasmtime](https://github.com/bytecodealliance/wasmtime) | `b738a89` | Apache-2.0 / LICENSE | .rs 2153, .cc 60, .c 28 | Installation |
| [bytecodealliance/wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) | `e3b53be` | Apache-2.0 / LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT | .rs 289, .cpp 67, .c 60 | About |
| [capnproto/capnproto-rust](https://github.com/capnproto/capnproto-rust) | `d53310e` | MIT / LICENSE | .rs 156 | Cap'n Proto for Rust |
| [djkoloski/rkyv](https://github.com/djkoloski/rkyv) | `d963abc` | MIT / LICENSE | .rs 169 | Resources |
| [google/flatbuffers](https://github.com/google/flatbuffers) | `5eb190f` | Apache-2.0 / LICENSE | .ts 198, .rs 169, .py 129 | Quick Start |
| [hashicorp/go-plugin](https://github.com/hashicorp/go-plugin) | `415e29b` | MPL-2.0 / LICENSE | .go 81, .py 7 | Go Plugin System over RPC |
| [hyperium/tonic](https://github.com/hyperium/tonic) | `c7e68c5` | MIT / LICENSE | .rs 510, .cc 2, .py 1 | Overview |
| [tokio-rs/prost](https://github.com/tokio-rs/prost) | `894f4a7` | Apache-2.0 / LICENSE | .rs 107 | *PROST!* |

### Local inference and code intelligence

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [apple/coremltools](https://github.com/apple/coremltools) | `a207b1a` | BSD-3-Clause / LICENSE.txt | .py 800, .cc 314, .java 253 | Install |
| [facebookincubator/Glean](https://github.com/facebookincubator/Glean) | `92ba871` | NOASSERTION / LICENSE | .cpp 283, .py 92, .java 56 | Glean |
| [ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) | `8e23278` | MIT / LICENSE, licenses | .cpp 804, .ts 742, .py 272 | llama.cpp |
| [huggingface/candle](https://github.com/huggingface/candle) | `9c1fcff` | Apache-2.0 / LICENSE-APACHE, LICENSE-MIT | .rs 635, .py 35, .js 14 | candle |
| [ml-explore/mlx](https://github.com/ml-explore/mlx) | `2e094ae` | MIT / LICENSE | .cpp 243, .py 135, .js 2 | MLX |
| [ml-explore/mlx-lm](https://github.com/ml-explore/mlx-lm) | `6fe0ce4` | MIT / LICENSE | .py 736, .js 2 | MLX LM |
| [quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy) | `c005b68` | MIT / LICENSE | .rs 529, .py 1 | Fast full-text search engine library written in Rust |
| [scip-code/scip](https://github.com/scip-code/scip) | `8fe5037` | Apache-2.0 / LICENSE | .go 55, .java 37, .kt 15 | SCIP Code Intelligence Protocol |

## 3. Licence consequences for this research

- **AGPL-3.0**: `QuantumNous/new-api`, `Ve-ria/CLIProxyAPIPlus` (the clone also has `LICENSE-MIT`; which files fall under which is not verified). Copyleft over network use: ideas only, no code reuse, no linking, no vendoring.
- **GPL-3.0**: `Ve-ria/quickx`, `limecloud/lime`. Same: study only.
- **No licence detected**: `stark-shadow/ecpa-plugins-store`, `hank9999/proxycast`, `BenedictKing/kiro.rs`. Without a licence the default is "all rights reserved", so nothing may be copied.
- **`facebookincubator/Glean`**: GitHub reports `NOASSERTION`; the licence text itself must be read from the `LICENSE` file before any statement (done in deliverable 11, not here).
- **MIT, Apache-2.0, BSD-3-Clause, MPL-2.0** (`hashicorp/go-plugin` is MPL-2.0, file-level copyleft): permissive or weak copyleft; reuse of a library still goes through `cargo deny` (`deny.toml`) and `THIRD-PARTY-NOTICES.txt`. Not verified against `deny.toml` here.

Deliverable 14 holds the full security and licensing assessment.


## 4. Capability matrix

Legend for repository columns: `Y` implemented in code (code read), `P` partial or limited, `N` searched and not present, `D` documented only (README or docs claim, code not read), `U` not examined, `.` no row written for that dimension. The **OMP2** column comes from the OMP2 audits ([deliverable 4](04-existing-vs-missing-matrix.md)) on commit `85d219ae7f`: `Y` Implemented, `P` Partial, `R` Prototype (unwired, test-only or opt-in), `A` Proposed (ADR text only), `N` Absent, `.` not audited. Dimension IDs are defined in [`evidence/01-method-and-dimension-ids.md`](evidence/01-method-and-dimension-ids.md).

### 4.1 Inference gateways

Rows: registry, authentication, scheduling, translation, interfaces, efficiency, observability. Sources: [group A notes](evidence/ref-gateways-a.md) (CLIProxyAPI, CLIProxyAPIPlus, OmniRoute; a single provider-runtime agent wrote 62 rows each, so authentication rows are mostly `.`) and [group B notes](evidence/ref-gateways-b.md).

| ID | Dimension |  OMP2 |CLIProxyAPI | CLIProxyAPIPlus | OmniRoute | 9router | freellmapi | llmhub | new-api | ProxyCast |
|---|---|---|---|---|---|---|---|---|---|---|
| A01 | API keys | Y | . | . | . | P | Y | Y | . | Y |
| A02 | OAuth | Y | . | . | . | Y | U | Y | . | Y |
| A03 | device authorization | Y | . | . | . | U | U | Y | . | Y |
| A04 | official CLI delegation | P | . | . | . | U | U | N | . | P |
| A05 | credential references (secret never inline) | Y | . | . | . | N | P | N | . | P |
| A06 | OS keychain | P | . | . | . | N | N | N | . | N |
| A07 | refresh coordination | P | . | . | . | U | U | Y | . | P |
| A08 | credential expiration handling | Y | . | . | . | D | P | Y | . | Y |
| A09 | credential recovery | P | . | . | . | U | U | Y | . | U |
| A10 | independent credential keepers | P | . | . | . | U | U | Y | . | U |
| A11 | authorized subscription entitlements | P | . | . | . | U | U | P | . | P |
| A12 | account and regional restrictions | P | . | . | . | U | U | P | . | U |
| E01 | prompt-cache awareness | Y | U | U | P | P | U | P | . | U |
| E02 | session affinity for cache reuse | P | U | U | U | U | U | Y | . | U |
| E03 | incremental response chaining | Y | U | U | U | U | U | P | . | U |
| E04 | provider-side state | Y | U | U | U | U | U | P | . | U |
| E05 | context retransmission handling | Y | U | U | P | U | U | P | . | U |
| E06 | semantic compression | P | U | U | P | Y | P | N | . | U |
| E07 | batch inference | N | U | U | P | U | U | Y | . | U |
| E08 | connection pooling | Y | U | U | U | U | U | U | . | P |
| E09 | streaming backpressure | P | U | U | U | U | U | U | . | U |
| E10 | token accounting | Y | U | U | U | P | P | Y | . | Y |
| E11 | cache accounting | Y | U | U | U | U | U | Y | . | U |
| E12 | time-to-first-token and latency measurement | Y | U | U | U | U | U | P | . | U |
| G01 | canonical provider identity | Y | P | Y | U | Y | Y | P | . | P |
| G02 | canonical model identity | Y | U | U | U | P | U | P | . | P |
| G03 | model aliases | Y | U | U | Y | Y | Y | Y | . | Y |
| G04 | provider-specific deployments | P | U | U | U | P | P | Y | . | P |
| G05 | dynamic discovery | Y | P | U | P | U | U | Y | . | U |
| G06 | hosted model catalogs | R | U | U | U | P | D | U | . | U |
| G07 | catalog synchronization | P | U | U | U | P | U | P | . | U |
| G08 | provider capability metadata | Y | P | U | U | Y | Y | Y | . | P |
| G09 | authentication requirements declared per pr... | Y | . | . | . | U | P | Y | . | Y |
| G10 | free-tier and anonymous discovery | P | . | . | . | D | Y | N | . | U |
| G11 | connection identity and deduplication | P | . | . | . | U | U | P | . | U |
| I01 | HTTP | Y | Y | U | U | Y | Y | Y | . | Y |
| I02 | SSE | Y | P | U | U | Y | Y | Y | . | Y |
| I03 | WebSocket | Y | Y | P | U | U | U | N | . | Y |
| I04 | persistent inference sessions | Y | P | U | U | U | U | N | . | U |
| I05 | gRPC | Y | U | U | U | U | U | N | . | U |
| I06 | Protobuf | Y | . | . | . | Y | N | N | . | U |
| I07 | AWS EventStream | Y | U | U | U | Y | U | N | . | Y |
| I08 | bidirectional streaming | Y | P | U | U | U | U | N | . | U |
| I09 | WebRTC | Y | U | U | U | U | U | N | . | U |
| I10 | HTTP/3 and QUIC | N | U | U | U | U | U | N | . | U |
| I11 | asynchronous inference jobs | P | U | U | U | U | P | Y | . | U |
| I12 | batch APIs | N | U | U | P | U | U | Y | . | U |
| I13 | native execution | Y | U | U | U | U | U | N | . | U |
| I14 | local IPC | Y | . | . | . | U | U | N | . | U |
| I15 | external agent-harness protocols | P | U | U | U | P | P | P | . | P |
| O01 | provider health | P | D | U | Y | U | Y | Y | . | Y |
| O02 | authentication health | P | . | . | . | U | U | Y | . | U |
| O03 | metrics (sessions, agents, tokens, cache, q... | P | U | P | P | P | P | P | . | P |
| O04 | cost accounting | Y | U | U | Y | P | P | Y | . | U |
| O05 | structured runtime events | Y | U | U | U | U | P | Y | . | Y |
| O06 | management surface (UI or API) | . | . | . | . | Y | Y | Y | . | Y |
| O07 | artifact reporting | . | . | . | . | U | U | P | . | P |
| O08 | deterministic execution status | . | . | . | . | N | N | N | . | N |
| S01 | deterministic provider selection | Y | Y | Y | Y | Y | Y | Y | . | Y |
| S02 | credential eligibility | Y | . | . | . | Y | Y | Y | . | Y |
| S03 | availability tracking | P | Y | U | Y | Y | Y | Y | . | Y |
| S04 | rate-limit awareness | P | Y | U | Y | P | P | Y | . | U |
| S05 | session affinity | P | P | Y | Y | P | U | Y | . | U |
| S06 | capability-aware routing | Y | U | U | P | Y | Y | Y | . | P |
| S07 | cost-aware routing | P | U | U | Y | D | D | Y | . | U |
| S08 | latency-aware routing | R | U | U | Y | U | Y | Y | . | U |
| S09 | circuit breakers | N | N | N | Y | Y | Y | Y | . | P |
| S10 | backpressure | Y | P | U | Y | U | P | Y | . | U |
| S11 | cancellation | Y | U | U | U | U | U | Y | . | U |
| S12 | retry policies | Y | P | U | P | Y | Y | Y | . | P |
| S13 | failure classification | Y | P | U | P | Y | P | Y | . | Y |
| S14 | quota-aware scheduling | P | P | U | Y | U | P | Y | . | U |
| T01 | OpenAI Chat Completions | Y | Y | Y | U | Y | Y | Y | . | Y |
| T02 | OpenAI Responses | Y | Y | U | U | Y | Y | Y | . | P |
| T03 | Anthropic Messages | Y | Y | U | U | Y | Y | Y | . | Y |
| T04 | Gemini | Y | Y | U | U | Y | Y | P | . | P |
| T05 | provider-specific protocols | Y | Y | U | U | Y | Y | Y | . | Y |
| T06 | reasoning normalization | Y | P | U | P | U | P | Y | . | P |
| T07 | tool-call correlation | Y | U | U | U | U | P | Y | . | U |
| T08 | streaming event normalization | Y | Y | U | U | Y | U | Y | . | Y |
| T09 | multimodal content | P | U | U | U | Y | Y | Y | . | U |
| T10 | structured output | P | U | U | U | U | P | P | . | U |
| T11 | error normalization | Y | U | U | U | Y | U | Y | . | U |
| T12 | translation architecture (pairwise translat... | Y | Y | Y | P | Y | Y | Y | . | Y |

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
| C02 | SCIP import | . | . | . | . | . | . | Y | D | . |
| C03 | SCIP export | . | . | . | . | . | . | Y | . | . |
| C04 | Glean | . | . | . | . | . | . | . | D | . |
| C05 | tree-sitter | . | . | . | . | . | . | . | U | . |
| C07 | tgrep or native trigram search | . | . | . | . | . | . | . | . | P |
| C08 | Tantivy | . | . | . | . | . | . | . | . | Y |
| C09 | native text search | . | . | . | . | . | . | . | . | Y |
| C10 | structural summaries | . | . | . | . | . | . | P | . | . |
| C15 | CodeIR | . | . | . | . | . | . | N | . | . |
| C16 | ChangeIR | . | . | . | . | . | . | N | . | . |
| C18 | sharing across agents | . | . | . | . | . | . | P | D | P |
| C19 | revision and document-version consistency | . | . | . | . | . | . | P | D | P |
| C20 | incremental invalidation | . | . | . | . | . | . | D | . | Y |
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
| O01 | provider health | P | OmniRoute, freellmapi, llmhub, ProxyCast |
| O06 | management surface (UI or API) | . | 9router, freellmapi, llmhub, ProxyCast |
| S03 | availability tracking | P | CLIProxyAPI, OmniRoute, 9router, freellmapi, llmhub, ProxyCast |
| S04 | rate-limit awareness | P | CLIProxyAPI, OmniRoute, llmhub |
| S05 | session affinity | P | CLIProxyAPIPlus, OmniRoute, llmhub |
| S07 | cost-aware routing | P | OmniRoute, llmhub |
| S08 | latency-aware routing | R | OmniRoute, freellmapi, llmhub |
| S09 | circuit breakers | N | OmniRoute, 9router, freellmapi, llmhub |
| S14 | quota-aware scheduling | P | OmniRoute, llmhub |
| T09 | multimodal content | P | 9router, freellmapi, llmhub |

These line up with the provider cards PRV-01 to PRV-03, PRV-06, PRV-07 and PRV-16 ([deliverable 5](05-provider-integration-recommendations.md)): availability and health evidence, rate-limit and quota observation, session affinity for cache reuse, cost- and latency-aware ranking, circuit breakers and metrics. The OMP2 types for most of them exist; the provider audit found the production producers missing. The list is a prompt for reading the cards, not a verdict: a `Y` in a reference repository says nothing about whether the behaviour is safe or allowed (section 3 and [deliverable 14](14-security-and-licensing-assessment.md)).

**Other points the notes establish** (citations in the linked notes):

- CLIProxyAPI and CLIProxyAPIPlus translate with pairwise translators; OMP2's canonical turn needs one codec per wire protocol ([PRV-14](06-transport-and-protocol-recommendations.md)).
- CLIProxyAPI reads quota from response headers, the producer OMP2 lacks ([PRV-02](05-provider-integration-recommendations.md)); OmniRoute scopes a circuit breaker per connection ([PRV-03](05-provider-integration-recommendations.md)).
- CLIProxyAPI's plugin ABI is `dlopen` plus JSON buffers with SHA-256 integrity and no unloading; CLIProxyAPIPlus has no plugin host (static Go registration); `hashicorp/go-plugin` contributes two ideas OMP2 lacks, version negotiation across a set of API levels and a health probe ([deliverable 7](07-abi-independent-extension-assessment.md)).
- None of the seven provider-implementation repositories has an OS keychain or encrypts credentials at rest ([deliverable 5b](05b-authentication-and-credential-lifecycle.md)).
- Patterns built around usage limits, client identity or shared subscriptions are catalogued in [deliverable 14](14-security-and-licensing-assessment.md), not as capabilities to adopt.

## 6. Limits

- Depth varies by repository and is stated in the reading notes. Several rows are `U` or `D`; large repositories (OmniRoute, new-api, llama.cpp, mlx) were read selectively.
- `QuantumNous/new-api` and `fallencolors/freellmapi` were read last, by a separate agent; their columns may be thin. `hashicorp/go-plugin`, `wasmtime` and `wit-bindgen` are libraries, so only extension rows apply.
- A `Y` is not a quality grade, and no performance claim about any repository is made.

Back to the [index](README.md).
