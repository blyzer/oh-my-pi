# Security and licensing assessment

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 14. Audited commit `85d219ae7f`. Research proposal: nothing here is implemented.

## 1. Stance

1. **Provider terms are a hard constraint** (owner section 5). Routing may spread load across credentials a user is entitled to use. It must not be used to circumvent usage limits, to pool one subscription across other people, to impersonate an official client, or to reach a private interface a provider has not authorised. Patterns of that kind found in the reference repositories are described neutrally below and classified `Rejected for security, licensing or operational reasons`.
2. **Security is preserved or strengthened, never traded for lightness** (`docs/adr/0049-tiered-integration-architecture.md`, D4): raw provider tokens stay in core, extensions get only scoped credentials, effects are declared, every mutation is approval-gated in core, sandbox egress per host is unchanged. No capability in this set may bypass those.
3. **Reference repositories are untrusted input.** None of their code was run, no dependency was installed, and no instruction found in their text was followed. Some are AGPL-3.0 or GPL-3.0: ideas only, nothing copied.
4. **No new default network listener, no mandatory dashboard, no database server.** Any capability that needs one is optional and out of the default build (owner section 13).

## 2. Licence assessment

Licences are from the GitHub API and the files in the pinned clones ([`evidence/ref-inventory.md`](evidence/ref-inventory.md)). This is an engineering reading of licence names, not legal advice.

| Licence | Repositories | Consequence for OMP2 |
|---|---|---|
| AGPL-3.0 | `QuantumNous/new-api`; `Ve-ria/CLIProxyAPIPlus` (clone also contains `LICENSE-MIT`; which files fall under which was not determined) | study only; no code, no linking, no vendoring, no copied schemas or prompts |
| GPL-3.0 | `Ve-ria/quickx`, `limecloud/lime` | study only |
| No licence detected | `stark-shadow/ecpa-plugins-store`, `hank9999/proxycast`, `BenedictKing/kiro.rs` | all rights reserved by default; nothing may be copied |
| MPL-2.0 | `hashicorp/go-plugin` | file-level copyleft; as a design reference only (OMP2 has no Go extension runtime); `deny.toml` allows MPL-2.0 only as package-scoped exceptions |
| `NOASSERTION` | `facebookincubator/Glean` | the `LICENSE` text must be read before any reuse; see deliverable 11 |
| MIT, Apache-2.0, BSD-3-Clause | the remaining repositories | libraries may be adopted through the normal review |

OMP2's own gate: `deny.toml` (cargo-deny) evaluates every target and `all-features = true`, allows only permissive licences (`0BSD`, `Apache-2.0`, `BSD-2-Clause`, `BSD-3-Clause`, `BSL-1.0`, `CC0-1.0`, `ISC`, `MIT`, `Zlib` and a few others), grants `MPL-2.0` to five named crates only, and denies unknown registries and git sources except one (`[sources]`). A GPL or AGPL crate would therefore fail the existing gate; an optional-feature dependency is covered because of `all-features`. `THIRD-PARTY-NOTICES.txt` and `about.toml` carry the notices. This was read, not run.

## 3. Patterns in the references that OMP2 must not adopt

Neutral descriptions, from the reading notes ([group B](evidence/ref-gateways-b.md), [group A](evidence/ref-gateways-a.md)); the cited sections hold the `repo@sha7:path` evidence.

| Pattern | Seen in | Why it is rejected | Card |
|---|---|---|---|
| Subscription pooling with automatic hand-over, plan-window kickoff messages, automatic reset-credit redemption, CLI identity headers and embedded C... | 1. nicolaeser/llmhub | Designed around provider usage limits and official-client identity; provider-terms risk. Rejected for security, licensing or operational reasons | see section |
| Borrowing tokens from other clients' credential files and presenting them through a local proxy | 2. hank9999/proxycast | Uses another product's entitlement outside its client; provider-terms risk. Rejected for security, licensing or operational reasons | see section |
| OS machine-identifier rewriting and browser-login interception | 2. hank9999/proxycast | Identity manipulation and credential capture; Rejected for security, licensing or operational reasons | see section |
| Stealth browser pools, TLS fingerprint mimicry, Cloudflare/Turnstile solving | 3. decolua/9router | Circumvents provider access controls. Rejected for security, licensing or operational reasons | see section |
| Local MITM with generated certificates for IDE traffic and model rewriting | 3. decolua/9router | Traffic interception and trust-store changes; Rejected for security, licensing or operational reasons | see section |
| Pooling or sharing one subscription across other principals, cloaking client identity, fingerprint mimicry, shared web sessions | CLIProxyAPI, CLIProxyAPIPlus, OmniRoute (group A notes) | circumvents provider terms or access controls | [PRV-09](05-provider-integration-recommendations.md) |
| Importing another application's credential store | CLIProxyAPI credential import already present in OMP2's broker command | credential reuse across applications may breach provider terms and copies a long-lived token into a second store | [AUTH-06](05b-authentication-and-credential-lifecycle.md) |
| Native shared-library plugin ABIs (`libloading`, `abi_stable`, `savefile-abi`, `cdylib`) | CLIProxyAPI plugin ecosystem (study of its native mechanism only) | fragile ABI, no isolation, contradicts ADR 0036; OMP2 has none and `libloading` is present only through `wgpu-hal` | [EXT-09](07-abi-independent-extension-assessment.md) |

## 4. Security notes per proposed capability

Taken from the `Security implications` line of each capability card (shortened). The card holds the full statement. A proposal with a materially new attack surface says so here.

| Card | Decision class | Security implication (shortened) |
|---|---|---|
| [PRV-01](05-provider-integration-recommendations.md) | Partially implemented: extend | Health evidence is credential-free by type (`plan.rs:399`). It must not expose account ids; keep it route-scoped. |
| [PRV-02](05-provider-integration-recommendations.md) | Partially implemented: extend | Headers can carry account identifiers; store only window ids, reset times and remaining counts (the existing types already do). Must not be used to rotate across principals the user is not entitled to use (see PRV-09). |
| [PRV-03](05-provider-integration-recommendations.md) | Requires benchmarking | A breaker keyed by a shared host could be made to open by one noisy account; key by route and account. |
| [PRV-04](05-provider-integration-recommendations.md) | Suitable as an optional extension | Batch results arrive later and may be stored provider-side; document retention; credentials follow the normal lease path. |
| [PRV-05](05-provider-integration-recommendations.md) | Partially implemented: extend | A remote catalog is untrusted input that can change prices, context limits and capabilities and so influence routing. Keep additive-only, bounded, and never let it add routes or endpoints (the sanitizer's contract); require an explicit o... |
| [PRV-06](05-provider-integration-recommendations.md) | Partially implemented: extend | None beyond existing session binding. |
| [PRV-07](05-provider-integration-recommendations.md) | Partially implemented: extend | None. |
| [PRV-08](05-provider-integration-recommendations.md) | Unnecessary or redundant | Fan-out under one name hides which provider received the prompt; the current explicit chain keeps that visible. |
| [PRV-09](05-provider-integration-recommendations.md) | Rejected for security, licensin... | Credential sharing widens blast radius and breaks per-principal audit. |
| [AUTH-01](05b-authentication-and-credential-lifecycle.md) | Partially implemented: extend | the owner token is not secret; it must not derive from credentials. A crashed owner is recovered by lease expiry (30 s default, `refresh.rs:73-80`). No new attack surface. |
| [AUTH-02](05b-authentication-and-credential-lifecycle.md) | Requires ADR reconsideration | binding `reveal` or `import_oauth` would let an extension obtain or inject raw credentials and would contradict ADR 0049 D4; any binding must keep the grant host-held and project-layer extensions at no grant (ADR 0075 D13). |
| [AUTH-03](05b-authentication-and-credential-lifecycle.md) | Partially implemented: extend | child gains the parent's account set; the child must not widen `AccountPin` or the key-source mode (it inherits the parent's convars already); sandbox rules protecting `credentials.db` are unchanged. |
| [AUTH-04](05b-authentication-and-credential-lifecycle.md) | Missing and justified: propose | a locked or absent facility must fail closed and surface `KeychainRefused` (`store.rs:1332-1340`); no plaintext or file fallback inside the keychain mode (`key.rs:416` doc). |
| [AUTH-05](05b-authentication-and-credential-lifecycle.md) | Requires benchmarking | a keeper increases provider refresh traffic and keeps tokens warm indefinitely; it must refresh only accounts the user enabled and never retry a revoked token in a loop; failures use the typed `refresh_failed` and, per AUTH-07, a consist... |
| [AUTH-06](05b-authentication-and-credential-lifecycle.md) | Requires ADR reconsideration | imported refresh tokens move a long-lived credential into a second store; a token from another client id may violate provider terms or break that tool's session on refresh-token rotation (inference); the file on disk is plaintext until i... |
| [AUTH-07](05b-authentication-and-credential-lifecycle.md) | Partially implemented: extend | disabling a revoked credential reduces repeated use of a dead token; mistaken disable denies service, so the terminal set must be explicit. |
| [AUTH-08](05b-authentication-and-credential-lifecycle.md) | Partially implemented: extend | facts are metadata only; a wrong cached fact must degrade to the current behaviour (try and learn), never block permanently. |
| [AUTH-09](05b-authentication-and-credential-lifecycle.md) | Partially implemented: extend | events must stay secret-free (`RefreshReceipt` is already non-secret by construction). |
| [PRV-10](06-transport-and-protocol-recommendations.md) | Unnecessary or redundant | New network stack and proxy-bypass surface (QUIC does not pass HTTP proxies); the existing `ProxyDiscoveryRoutes` and proxy transport (`transport/proxy.rs`) would not cover it. |
| [PRV-11](06-transport-and-protocol-recommendations.md) | Requires benchmarking | Smaller default attack surface if voice is optional; the device-attestation feature is already coupled to `realtime` (`Cargo.toml:40-46`). |
| [PRV-12](06-transport-and-protocol-recommendations.md) | Unnecessary or redundant | None new. |
| [PRV-13](06-transport-and-protocol-recommendations.md) | Suitable as an optional extension | Opens a network listener that spends the user's credentials; must bind loopback by default and require a token, like the existing serve auth (`crates/serve/src/auth.rs`). |
| [PRV-14](06-transport-and-protocol-recommendations.md) | Already implemented: reuse | Typed events keep tool execution authorization on `ToolCallReady` only (`event.rs:218`). |
| [PRV-15](06-transport-and-protocol-recommendations.md) | Partially implemented: extend | None. |
| [PRV-16](06-transport-and-protocol-recommendations.md) | Missing and justified: propose | Keep route ids, never account ids, in metric labels. |
| [PRV-17](06-transport-and-protocol-recommendations.md) | Requires benchmarking | Unbounded buffering is a denial-of-service on the local process. |
| [IPC-01](06b-ipc-and-serialization.md) | Already implemented: reuse | Unchanged. Frame ceilings exist at every Protobuf boundary (`crates/proto/src/bounds.rs:11` `FRAME_MAX_BYTES`, `crates/app/src/daemon.rs:63` `RPC_MESSAGE_MAX_BYTES`). |
| [IPC-02](06b-ipc-and-serialization.md) | Requires benchmarking | A binary frame needs the same bound checks as today (`MAX_VECTOR_DIMENSIONS`, row count, finiteness). Raw f32 must still reject NaN and infinity. |
| [IPC-03](06b-ipc-and-serialization.md) | Partially implemented: extend | Must keep first-chunk-only hash and size rules and the 64 MiB cap; staging files need the existing fsync and rename discipline in `crates/journal/src/blob.rs`. |
| [IPC-04](06b-ipc-and-serialization.md) | Requires benchmarking | Zero-copy readers touch untrusted bytes in place; every candidate needs a verifier (see ref-serialization evidence). Protobuf plus CAS keeps the existing hash check. |
| [IPC-05](06b-ipc-and-serialization.md) | Requires ADR reconsideration | Zero-copy access to untrusted bytes without a verifier is memory-unsafe or panics; FlatBuffers and rkyv expose unchecked entry points. |
| [IPC-06](06b-ipc-and-serialization.md) | Partially implemented: extend | JSON with a 64 MiB cap is parsed in the Rust parent from a sandboxed child; that is the existing trust boundary. |
| [IPC-07](06b-ipc-and-serialization.md) | Unnecessary or redundant | A new parser on untrusted input and a new dependency. |
| [IPC-08](06b-ipc-and-serialization.md) | Unnecessary or redundant | Shared segments cross sandbox boundaries and weaken isolation; mmap of a file that another process truncates can fault. |
| [IPC-09](06b-ipc-and-serialization.md) | Suitable as an optional extension | The frame reader change is a hardening item; compression on TLS carries a known side-channel class for secrets in compressed payloads, so it must stay off for credential-bearing RPCs. |
| [EXT-01](07-abi-independent-extension-assessment.md) | Already implemented: reuse | Sandboxed tier has no network and scoped filesystem (`spawn.rs:553-577`); trusted tier has no sandbox (`spawn.rs:578`). ADR 0076 (Proposed) covers refinement. |
| [EXT-02](07-abi-independent-extension-assessment.md) | Requires ADR reconsideration | Adds executable code distribution. ADR 0046 item 5 states "Foreign executables remain unloaded (ADR 0036)"; any materializer must run under the workspace-trust and `plugin_command` approval model. |
| [EXT-03](07-abi-independent-extension-assessment.md) | Partially implemented: extend | Strengthens the isolation claim in section 5. |
| [EXT-04](07-abi-independent-extension-assessment.md) | Partially implemented: extend | None adverse. |
| [EXT-05](07-abi-independent-extension-assessment.md) | Partially implemented: extend | Directly supports ADR 0076 scoped grants. |
| [EXT-06](07-abi-independent-extension-assessment.md) | Partially implemented: extend | None; must not expose credentials. |
| [EXT-07](07-abi-independent-extension-assessment.md) | Already implemented: reuse | Data-only module; credentials stay host-placed (ADR 0075). |
| [EXT-08](07-abi-independent-extension-assessment.md) | Suitable as an external integra... | The operator owns the process and its credentials; Provider terms apply to what that gateway does. |
| [EXT-09](07-abi-independent-extension-assessment.md) | Rejected for security, licensin... | Third-party native code in the engine process; contradicts the killable-child rule (`0036:64-65`) and the Rust ABI is unstable across compiler versions. |
| [EXT-10](07-abi-independent-extension-assessment.md) | Requires ADR reconsideration | Smaller attack surface per component than a Python host in principle; larger engine dependency surface. |
| [EXT-11](07-abi-independent-extension-assessment.md) | Requires ADR reconsideration | Executable distribution plus a sandbox profile for a non-Python process; ADR 0076 limits apply. |
| [EXT-12](07-abi-independent-extension-assessment.md) | Requires benchmarking | Zero-copy readers need strict validation. |
| [PY-01](08-python-runtime-comparative-assessment.md) | Already implemented: reuse | Child already isolates the interpreter from the agent process. Embedding does not add isolation by itself; isolation comes from the process and sandbox (see the answer in section 1). |
| [PY-02](08-python-runtime-comparative-assessment.md) | Requires benchmarking | none beyond running the existing child entries; run on an isolated checkout, do not run untrusted extensions. |
| [PY-03](08-python-runtime-comparative-assessment.md) | Requires benchmarking | none: benchmark runs on local fixtures. |
| [PY-04](08-python-runtime-comparative-assessment.md) | Already implemented: reuse | The external interpreter runs the user's Python with the same sanitized environment and optional sandbox wrapper (`crates/envd/src/eval/process.rs:586 command_for`); it adds dependence on a host interpreter, which ADR 0036 decision 1 for... |
| [PY-05](08-python-runtime-comparative-assessment.md) | Requires ADR reconsideration | Larger read allow-list in the sandboxed tier; a second distribution artifact to verify (`crates/ext/src/trust.rs` covers extensions, not interpreters). |
| [PY-06](08-python-runtime-comparative-assessment.md) | Requires ADR reconsideration | New large dependency in the trusted computing base; interpreter-level sandbox escapes are untested. |
| [PY-07](08-python-runtime-comparative-assessment.md) | Suitable as an external integra... | Same as MCP stdio today: unsandboxed unless wrapped by the exec sandbox; credentials only through declared grants. |
| [PY-08](08-python-runtime-comparative-assessment.md) | Requires ADR reconsideration | Smaller syscall surface by construction, but a new large dependency (cranelift, wasmtime) enters the trusted computing base and the extension contract changes. |
| [PY-09](08-python-runtime-comparative-assessment.md) | Unnecessary or redundant | not applicable. |
| [PY-10](08-python-runtime-comparative-assessment.md) | Partially implemented: extend | none. |
| [SWM-01](09-single-session-swarm-assessment.md) | Already implemented: reuse | Same trust boundary as the root; mitigated by per-child CoW view and inherited workspace trust (`crates/driver/src/subagent/spawn.rs:113 workspace_trust`). |
| [SWM-02](09-single-session-swarm-assessment.md) | Partially implemented: extend | none new if phases keep per-phase isolation. |
| [SWM-03](09-single-session-swarm-assessment.md) | Unnecessary or redundant | a Foreman with write authority over scheduling would widen the prompt-injection surface. |
| [SWM-04](09-single-session-swarm-assessment.md) | Already implemented: reuse | forced termination is journaled as uncertainty, not success (`crates/agent/src/cancel.rs:1`). |
| [SWM-05](09-single-session-swarm-assessment.md) | Partially implemented: extend | none; bounding reduces runaway risk. |
| [SWM-06](09-single-session-swarm-assessment.md) | Requires benchmarking | none. |
| [SWM-07](09-single-session-swarm-assessment.md) | Requires benchmarking | a child process needs a scoped token, not provider credentials (ADR 0039 B6 states no provider credentials reach workers). |
| [SWM-08](09-single-session-swarm-assessment.md) | Requires benchmarking | credential keepers must stay single-owner; duplication of refresh logic would be a defect. |
| [SWM-09](09-single-session-swarm-assessment.md) | Partially implemented: extend | none. |
| [SWM-10](09-single-session-swarm-assessment.md) | Unnecessary or redundant | shares the `.git` object store with the parent. |
| [SWM-11](09-single-session-swarm-assessment.md) | Suitable as an external integra... | foreign harnesses may bypass approvals (cotion passes a flag that disables approvals and sandbox, `Ve-ria/cotion@b7a4307:src/run.ts`); OMP2 must keep its own sandbox and verdict. |
| [SWM-12](09-single-session-swarm-assessment.md) | Unnecessary or redundant | a second state store would split durable truth away from the journal. |
| [SWM-13](09-single-session-swarm-assessment.md) | Partially implemented: extend | a pid check must use process identity (`crates/envd/src/process_identity.rs`) to avoid pid reuse. |
| [SWM-14](09-single-session-swarm-assessment.md) | Rejected for security, licensin... | GPL-3.0 code cannot be copied into this tree; grape's model is a hosted multi-tenant service. |
| [INF-01](10-local-inference-assessment.md) | Partially implemented: extend | The worker runs a native ONNX Runtime in a child process; frames are size-capped and JSON-validated (`crates/memory/src/embedding/protocol.rs:12`). Model ids are charset-validated. Model download comes from Hugging Face at first use. |
| [INF-02](10-local-inference-assessment.md) | Missing and justified: propose | A per-machine endpoint must be per-user, mode 0600 and verify the peer; a shared worker must not see another user's text. |
| [INF-03](10-local-inference-assessment.md) | Suitable as an external integra... | Server is an unauthenticated loopback HTTP listener by default; OMP must keep it on loopback, treat output as untrusted and never send secrets to it. The user, not OMP, installs the binary. |
| [INF-04](10-local-inference-assessment.md) | Requires ADR reconsideration | A C++ parser of untrusted GGUF files would run in the agent process, with no isolation. |
| [INF-05](10-local-inference-assessment.md) | Suitable as an optional extension | Python process with network access to model hubs; run it under the extension permission model. |
| [INF-06](10-local-inference-assessment.md) | Requires benchmarking | Compiled model artifacts need the same SHA-256 verification as `ArtifactStore` (`crates/ai/src/local/artifact.rs`). |
| [INF-07](10-local-inference-assessment.md) | Requires ADR reconsideration | ORT binaries are downloaded at build time over TLS; supply-chain exposure sits in the build, not at runtime. Pin and verify. |
| [INF-08](10-local-inference-assessment.md) | Requires benchmarking | Pure Rust; safetensors loading avoids pickle risk. |
| [INF-09](10-local-inference-assessment.md) | Requires benchmarking | Pinned local models must never silently fall back to hosted ones (ADR 0023 rule 4). |
| [INF-10](10-local-inference-assessment.md) | Missing and justified: propose | None beyond not widening the surface; placement never routes local-pinned data to a remote. |
| [INF-11](10-local-inference-assessment.md) | Partially implemented: extend | Candidate memory text goes to the model; local only. |
| [INF-12](10-local-inference-assessment.md) | Partially implemented: extend | Downloaded binaries need pinning. |

## 5. Invariants this set asks the owner to keep

1. Credentials: the refresh fence and owner identity must work across processes ([AUTH-01](05b-authentication-and-credential-lifecycle.md)); a locked keychain must fail closed; no plaintext fallback inside a keychain mode.
2. Extensions: Ed25519-signed index, exact capability grants, host-placed credentials (ADR 0075), declared effects. Manifest codes that are declared but never produced ([EXT-03 to EXT-05](07-abi-independent-extension-assessment.md)) are a gap in enforcement, not in design.
3. Local servers (llama-server, MLX server): loopback only, no default exposure, treated as untrusted model output sources; this is a condition on INF-03 and INF-05, not an assumption that it already holds.
4. The `omp.gateway.v1.ForwardProxy` surface already requires bearer authentication and an allowlisted request vocabulary (`crates/proto/proto/omp/gateway/v1/gateway.proto:29`); an OpenAI-compatible facade ([PRV-13](06-transport-and-protocol-recommendations.md)) must not widen it.
5. WebRTC and live voice are in the default `omp-app` build (`crates/app/Cargo.toml:94`); their attack surface is accepted today and is not changed by anything proposed here.

## 6. Not verified

- Legal review of any licence; the AGPL clause scope for the `CLIProxyAPIPlus` dual files.
- Whether any reference repository contains malicious code (none was executed or scanned).
- Dynamic behaviour of any OMP2 credential path (no tests were run).

Back to the [index](README.md).
