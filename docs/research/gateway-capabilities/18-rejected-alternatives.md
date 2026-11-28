# Explicitly rejected alternatives and reasons

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 18. Audited commit `85d219ae7f`. Research proposal: nothing here is implemented.

Three kinds of entry: capabilities **closed** (`Unnecessary or redundant`, `Rejected for security, licensing or operational reasons`), capabilities **held** because they need an ADR decision first (`Requires ADR reconsideration`), and **reference-repository patterns** that were read and not adopted. Reasons are the cards' own, shortened; the card has the evidence.

## 1. Closed

| Card | Alternative | Decision class | Why | What would reopen it |
|---|---|---|---|---|
| [PRV-08](05-provider-integration-recommendations.md) | Cross-provider virtual model names | Unnecessary or redundant | Exact provider-scoped aliases (`crates/catalog/src/compile.rs:1048`, `snapshot.rs:716`), ordered `FallbackChain` with revert policy (`crates/catalog/src/resolve.rs:84,...  | not applicable. |
| [PRV-09](05-provider-integration-recommendations.md) | Credential pooling across other principals | Rejected for security, lice... | OMP2 rotates among accounts the user holds and can restrict rotation to one principal (`RotationPolicy.preserve_principal`, `crates/ai/src/account/pool.rs:212`); `Acco... Credential sharing widens blast radius and breaks per-principal audit. | Rotation stays bounded to credentials the user is entitled to use; PRV-02 never feeds a policy that selects a principal the session was not pinned to. |
| [PRV-10](06-transport-and-protocol-recommendations.md) | HTTP/3 and QUIC | Unnecessary or redundant | `cargo tree --offline --locked --workspace -i quinn` and `-i h3` return "did not match any packages"; `Cargo.lock` lists `quinn` only as a dependency of `reqwest`, whi...  | Revisit only if a catalog provider documents HTTP/3 as required or materially faster. |
| [PRV-12](06-transport-and-protocol-recommendations.md) | gRPC as a provider transport | Unnecessary or redundant | OMP2 serves gRPC (`service Inference`, `crates/proto/proto/omp/inference/v1/inference.proto:39`, bidirectional `Turn` at `:48`, `Realtime` at `:58`; `crates/serve/Carg...  | Revisit if a catalog provider exposes only gRPC. |
| [IPC-07](06b-ipc-and-serialization.md) | MessagePack for internal JSON channels | Unnecessary or redundant | JSON channels today: embedding NDJSON, extension CONTROL, journal SSE+JSON, `omp rpc` NDJSON (`crates/rpc/src/framing.rs:1`). No `rmp` or `rmp-serde` in `Cargo.lock`.  | Reopen only if the IPC-02 benchmark shows MessagePack beats the best Protobuf or raw-f32 framing by a margin fixed beforehand. |
| [IPC-08](06b-ipc-and-serialization.md) | Shared memory and memory-mapped files for local bulk transfer | Unnecessary or redundant | No shared-memory or `memfd` use anywhere in `crates`, `extensions` or `packages` (searched `shm_open`, `shared_memory`, `memfd_create`, `ipc-channel`, `shmem`). `memma...  | Reopen when a measured workload spends a stated share of wall time in IPC copies. |
| [EXT-09](07-abi-independent-extension-assessment.md) | Dynamic shared-library plugins (`libloading`, `abi_stable`, `savefile-abi`, `... | Rejected for security, lice... | Absent (`evidence/omp2-extensions.md` section 7); a unit test forbids `libloading` and `dlopen` in skills discovery (`crates/driver/src/discovery/skills.rs:2128-2139`). Third-party native code in the engine process; contradicts the killable-child rule (`0036:64-65`) and the R... | No `libloading`, `abi_stable` or `savefile` appears in any workspace `Cargo.toml`; the existing grep-based tests stay. |
| [PY-09](08-python-runtime-comparative-assessment.md) | Go executable adapters as Python-runtime alternatives | Unnecessary or redundant | None found in OMP2. Searched: `git ls-files '*.go' go.mod`, grep for `go adapter`, `go executable`, `go binary` in `crates`, `docs`, `extensions`, `AGENTS.md`. Only a...  | Re-open only if a Go adapter appears in the tree. |
| [SWM-03](09-single-session-swarm-assessment.md) | Do not add a dedicated Foreman component | Unnecessary or redundant | `docs/adr/0039-remote-control-and-factory-modes.md:618` D3 (role split), `crates/driver/src/subagent/spawn.rs:1042 finish_isolation` (integration), `crates/envd/src/wo...  | foreman duties are listed in section 3 with owning code; no new crate named foreman. |
| [SWM-10](09-single-session-swarm-assessment.md) | Git worktree as an isolation backend | Unnecessary or redundant | `docs/adr/0007-subagent-filesystem-isolation.md:53` ("Prohibited: worktree-only isolation presented as full isolation"), `crates/envd/src/workspace/operations.rs:673 c...  | none unless B6 shows the copy fallback is unacceptable. |
| [SWM-12](09-single-session-swarm-assessment.md) | Do not add a swarm crate or a second scheduler | Unnecessary or redundant | `crates/driver/src/subagent/workpool_scheduler.rs:101 WorkpoolPolicy`, `crates/agent/src/jobs.rs:50 JobKind`, `crates/adw/src/run.rs:173 Run`. The three together hold...  | any later proposal for a new layer states which of the three it cannot extend, with evidence. |
| [SWM-14](09-single-session-swarm-assessment.md) | Adopt agent-control or dispatch code from the reference harnesses | Rejected for security, lice... | reference only: `limecloud/lime@015b878:lime-rs/crates/agent-graph-store/src/types.rs` (GPL-3.0), `grape-dev/grape@b31f8d8:src/domains/mgmt/org_dispatch.py` (MIT, Pyth... GPL-3.0 code cannot be copied into this tree; grape's model is a hosted multi-tenant service. | no code from these repositories appears in the OMP2 tree.  ## 8. Benchmark plan (all `not measured`)  No measurement was run for this deliverable.... |

## 2. Held for an ADR decision

These are not rejected. Each contradicts or reopens a recorded or owner-locked decision, so the evidence is collected and the decision is the owner's. None is scheduled.

| Card | Alternative | Decision it reopens | Missing evidence or blocker |
|---|---|---|---|
| [AUTH-02](05b-authentication-and-credential-lifecycle.md) | Decide the fate of the unbound extension credential CONTROL domain (`omp.cred... | an explicit decision. Either bind only a narrow metadata subset (`list`, `usage`, `report_block`) for user-layer extensions with a host grant, or d... | the ADR 0075 amendment in section 4 carries this decision. |
| [AUTH-06](05b-authentication-and-credential-lifecycle.md) | CLIProxyAPI credential import and credential reuse across applications | an owner decision, not code: whether an import of tokens minted by another tool's OAuth client for a user-held subscription is permitted, for which... | if removed, drop `CliProxyCredential` and the import tests in one cutover. |
| [IPC-05](06b-ipc-and-serialization.md) | Zero-copy formats (Cap'n Proto, FlatBuffers, rkyv): reconsider the unmeasured... | The owner brief asks for a benchmark and forbids adopting zero-copy on theory alone. ADR 0048 contains a rejection without data. Reference reading... | Class 6 follow-up: reopen only when a benchmark fires. |
| [EXT-02](07-abi-independent-extension-assessment.md) | Materialize or remove the `[[binaries]]` manifest field | A per-target executable artifact in the index and lock, with the same hash and signature checks as wheels, resolved to a path the LSP and DAP loade... | Option (2) is the smallest step and needs no ADR; take it first if a need appears. |
| [EXT-10](07-abi-independent-extension-assessment.md) | WebAssembly component extensions | A component host, WIT interface for tools, hooks and providers, a capability-import mapping onto DATA grants, and signing of components through the... | Optional extension only; not before EXT-02 to EXT-06. |
| [EXT-11](07-abi-independent-extension-assessment.md) | Native executable extension class speaking the CONTROL protocol | A published CONTROL contract, an executable artifact kind (EXT-02) and a non-Python host. | Prefer MCP unless callback overhead is shown to matter. |
| [PY-05](08-python-runtime-comparative-assessment.md) | Standalone CPython as the runtime for extension hosts and workers | Missing evidence that would justify reopening: (1) embedded child startup and RSS versus standalone on the same machine (PY-02); (2) callback gap v... | Do not start before PY-02 and PY-03 report. |
| [PY-06](08-python-runtime-comparative-assessment.md) | RustPython as an embedded alternative | Missing evidence: (1) whether RustPython can import the pinned pure-Python packages in `crates/py/requirements.txt` and the repo modules under `cra... | Do not start; documented only. |
| [PY-08](08-python-runtime-comparative-assessment.md) | WebAssembly components as an extension runtime | Missing evidence: (1) a WIT world for the `omp.*` surface and cost of mapping `Secret`, `EnvPath`, streams and DATA; (2) Python on Wasm that can im... | Do not start; documented only. |
| [INF-04](10-local-inference-assessment.md) | llama.cpp linked in process | Nothing is missing that INF-03 does not cover with a process boundary. An in-process binding would need the owner to reverse a locked decision. | Recommendation: do not reopen. |
| [INF-07](10-local-inference-assessment.md) | Confine ONNX Runtime to the out-of-process worker | An explicit ADR statement that ONNX Runtime is allowed inside the worker process only, plus removal or gating of the in-process adapter so the grap... | Documentation only for ONNX Runtime: docs read, code not read. |

## 3. Reference-repository patterns read and not adopted

Described neutrally in the reading notes with `repo@sha7:path` evidence: [group A](evidence/ref-gateways-a.md), [group B](evidence/ref-gateways-b.md). The security consequences are in [deliverable 14](14-security-and-licensing-assessment.md).

| Pattern | Source note section | Reason |
|---|---|---|
| Subscription pooling with automatic hand-over, plan-window kickoff messages, automatic reset-credit redemption, CLI identity headers and... | 1. nicolaeser/llmhub (`8bdc1de`) | Designed around provider usage limits and official-client identity; provider-terms risk. Rejected for security, licensing or operational reasons |
| Borrowing tokens from other clients' credential files and presenting them through a local proxy | 2. hank9999/proxycast (`3ad8ff6`) | Uses another product's entitlement outside its client; provider-terms risk. Rejected for security, licensing or operational reasons |
| OS machine-identifier rewriting and browser-login interception | 2. hank9999/proxycast (`3ad8ff6`) | Identity manipulation and credential capture; Rejected for security, licensing or operational reasons |
| Stealth browser pools, TLS fingerprint mimicry, Cloudflare/Turnstile solving | 3. decolua/9router (`5ae63d2`) | Circumvents provider access controls. Rejected for security, licensing or operational reasons |
| Local MITM with generated certificates for IDE traffic and model rewriting | 3. decolua/9router (`5ae63d2`) | Traffic interception and trust-store changes; Rejected for security, licensing or operational reasons |
| Provider kinds, cache-rate and tier pricing as code keyed on model-name regexes | 1. nicolaeser/llmhub (`8bdc1de`) | Violates providers-as-data; `cost.ts:CACHE_READ_RATES` is exactly what lintx rejects. Rejected |
| Untyped `JsonMap` translation | 1. nicolaeser/llmhub (`8bdc1de`) | Violates typed wire structs. Rejected |
| LLM judge for model equivalence in the registry | 1. nicolaeser/llmhub (`8bdc1de`) | Non-deterministic where a curated catalog or explicit alias works; at most an offline curation aid. Rejected as runtime behaviour |
| Required Postgres and Redis, Next.js console, mandatory web UI | 1. nicolaeser/llmhub (`8bdc1de`) | Contradicts a lightweight native harness; owner brief section 13 forbids a mandatory dashboard or database server. Unnecessary or redundant |
| Importing another gateway's auth files or management API to move accounts | 1. nicolaeser/llmhub (`8bdc1de`) | Credential-handling surface with no OMP2 use case. Rejected |
| Plaintext API keys in a local DB | 2. hank9999/proxycast (`3ad8ff6`) | Contradicts credential-reference practice; Rejected |
| Two parallel credential balancers plus an unused resilience pipeline | 2. hank9999/proxycast (`3ad8ff6`) | Second scheduler; Unnecessary or redundant |
| Model maps and supported-model lists in `.rs` | 2. hank9999/proxycast (`3ad8ff6`) | Violates providers-as-data (lintx reviewer-reject) |
| GPL-3.0 licensed code as a copy source | 2. hank9999/proxycast (`3ad8ff6`) | Licence conflict; study only |
| Client cloaking (billing header, CLI version, tool-name suffixes) | 3. decolua/9router (`5ae63d2`) | Official-client impersonation; Rejected |
| Multi-account rotation with proxy pools to maximise free or subscription quota | 3. decolua/9router (`5ae63d2`) | Pooling to stretch limits; Rejected |
| LLM judge "fusion" on the routing path | 3. decolua/9router (`5ae63d2`) | Non-deterministic and costly; Requires benchmarking at best, outside the owner's deterministic preference |
| Prompt-style compression modes (caveman, ponytail variants) injected into system prompts | 3. decolua/9router (`5ae63d2`) | Prompt changes break stable-prefix cache rules (volatile text in stable prefix) and are not deterministic filters; Unnecessary or redundant |

## 4. Structural alternatives considered and set aside

| Alternative | Why set aside | Evidence |
|---|---|---|
| A second provider runtime, scheduler, credential manager, memory system, config store or index service | owner constraint 8; the existing fixed stack, account pool, job board and caches already own these | [deliverable 2](02-verified-current-state.md) sections 5 and 8 |
| Pairwise protocol translators | OMP2 uses one canonical turn (`ChatEvent`, ADR 0022): N codecs, not N squared translators; the references that use pairwise translators carry 31 to 38 registration lines for it | [PRV-14](06-transport-and-protocol-recommendations.md) |
| A mandatory web dashboard, database server or distributed control plane | owner section 13; not required by any measured need | [deliverable 14](14-security-and-licensing-assessment.md) |
| Replacing ADR 0036 (embedded CPython) | no concrete evidence found; embedding is not what isolates extensions, but it removes the host-Python dependency | [deliverable 8](08-python-runtime-comparative-assessment.md) |

Back to the [index](README.md).
