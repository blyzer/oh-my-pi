# Verified current-state architecture of OMP2

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 2. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

This document says what OMP2 verifiably is on the audited commit, so the later documents can propose against facts. It separates what runs (**Implemented**), what exists but is gated or incomplete (**Partial**, **Prototype**), what only an ADR describes (**Proposed**), what sits on an unmerged branch (**Unmerged**) and what was searched for and not found (**Absent**). Per-area audits with more citations are in `evidence/` and are linked from section 12.

## 1. Scope, authority and method

| Item | Value |
|---|---|
| Authoritative checkout | `/Users/enverfrancisco/repositories/oh-my-pi`, remote `origin` = `git@github.com:blyzer/oh-my-pi.git`, branch `omp2`, `HEAD` = `85d219ae7f` = `origin/omp2` after `git fetch origin omp2` |
| Second checkout named in the request | `/Users/enverfrancisco/repositories/omp2` does not exist on this machine (`cd` returned "No such file or directory"), so it was not used |
| Files read | the research worktree `/private/tmp/omp-gateway-research` (branch `docs/research-gateway-capabilities`, created from `origin/omp2`), so every path below is relative to that tree |
| Open pull requests | none: `gh pr list -R blyzer/oh-my-pi --state open --json number \| jq length` printed `0` |
| What was done | reading code, `Cargo.toml`, `Cargo.lock`, ADRs, `docs/adr/STATUS.md`; `cargo tree --offline` (no compilation); a script that counts lines of `*.rs` under `crates/*/src` |
| What was not done | no workspace build, no tests, no benchmark in this document |

Commands whose output is quoted below, run in the worktree with `CARGO_TARGET_DIR=/private/tmp/omp-research-target-parent` (`cargo tree` compiles nothing):

```sh
cargo tree --offline --workspace -i libloading -e normal,build --depth 2
cargo tree --offline --workspace -i scip       -e normal,build --depth 2
cargo tree --offline --workspace -i webrtc     -e normal,build --depth 2
cargo tree --offline --workspace -i rkyv       -e normal,build --depth 2
cargo tree --offline --workspace -i ort        -e normal,build --depth 2   # error: did not match any packages
cargo tree --offline --workspace -i fastembed  -e normal,build --depth 2   # same error
cargo tree --offline --workspace -i candle-core -e normal,build --depth 2  # same error
```

## 2. ADR index and status

`docs/adr/` holds 78 numbered ADR files (two share the number 0049: `0049-ci-dyn-device-schema.md` and `0049-tiered-integration-architecture.md`) and a `0034/` directory of TLA+ models. `docs/adr/STATUS.md` has 76 rows. Counting the first words of the Status column: 25 Implemented, 26 Partially implemented, 21 Proposed, 2 Not started, 1 Accepted (ADR 0050), 1 Withdrawn (ADR 0071). The STATUS header says rows were last verified at different dates and commits (2026-10-04 at `083b38fe7d` for the base, later passes for individual rows), so a row is evidence of what was true on its date, not on `85d219ae7f`.

ADRs that decide the areas of this research:

| ADR | Decision (title) | STATUS row | Role in this research |
|---|---|---|---|
| 0018 | A provider is shared infrastructure, not a `stream` function | Implemented | provider runtime shape, credential kinds and leasing |
| 0022 | An adapter is complete when it yields one canonical turn | Implemented | canonical intermediate representation for protocol translation |
| 0036 | Embedded Python for extensions, `@remote`, and `Eval` | Partially implemented | locked Python runtime decision |
| 0043 | Native provider CLIs are delegated agent executors, not direct model transports | Partially implemented | official-CLI delegation |
| 0045 | Envd responsibility boundaries | Partially implemented | what the host owns |
| 0046 | Recover v1 extension properties without routing every feature through envd | Partially implemented | extension management |
| 0048 | Schema and serialization boundaries | Partially implemented | wire formats |
| 0049 (tiered) | Tiered integration architecture | Partially implemented | placement rule T0 to T3 |
| 0050 | Extension-declared URL schemes | Accepted | extension surface |
| 0051 | ADW runs are durable, resumable and extensible over `omp-journal` | Proposed | workflow engine |
| 0052 | One executor contract: native OMP2, delegated agents and external harnesses | Partially implemented | external harness boundary |
| 0059 | Missing upstream providers as catalog data; eight sign-in flows | Proposed (S1, S2 landed) | provider additions, credential flows |
| 0069 | No generated gRPC clients now; protos compiled by one documented `protox` command | Proposed | Protobuf and gRPC stance |
| 0074 | Anthropic server-side advisor tool as an opt-in `HostedTool::Advisor` | Proposed | tension with "no vendor server-side tools" |
| 0075 | Extension HTTP credentials are placed by the host | Partially implemented | extension credential model |
| 0076 | Extension permissions get a middle level | Proposed | extension permissions |

The other in-scope ADRs (0007, 0010, 0011, 0023, 0025, 0039, 0040, 0042, 0053, 0056, 0060, 0062, 0071, 0073) are read in the area audits, not re-read here.

### 2.1 Owner-locked decisions that bound every proposal

`AGENTS.md`, section "Locked Deviations from pi (owner decisions)" (lines 526 to 582), fixes these, among others:

- Extensions and `Eval`: embedded free-threaded CPython only, no JavaScript or TypeScript plugin runtime, stdlib frozen in the binary.
- Inference: providers-as-data (catalog), no model-name conditionals in `.rs` files, typed serde or prost wire structs, provider frames decode to canonical semantic events, **no vendor server-side tools** (ADR 0074 proposes one exception; see section 12).
- Runtime: tokio and rayon only; local audio and ML via candle, never C or C++ binding graphs (`whisper-rs`, `llama-cpp`).
- Feature graphs earn their weight; cold `cargo run --bin omp` build time is a gate.
- Control plane: Directors and Components, no regimes; the agent loop is a generic hook surface.

ADR 0049 adds the placement rule: every capability lives in the lowest tier that satisfies it (T0 authority core `omp-envd`; T1 generic capability crates behind `cap-<name>` features; T2 first-party extensions in `extensions/`; T3 third-party), new capabilities default to T2, vendor integration code never enters T0 or T1, and inference and search transports stay as providers-as-data (`docs/adr/0049-tiered-integration-architecture.md`, D1 and D6). Every capability card in this research must fit these rules or say it needs an ADR change.

## 3. Workspace map

`crates/` has 62 member crates; the script count is 1,325,283 lines of `*.rs` under `crates/*/src`. Largest by that count: `omp-envd` 220,051; `omp-ai` 175,352; `omp-tui` 98,180; `omp-driver` 92,071; `omp-shell-builtins` 92,078; `omp-chat` 87,934; `omp-tools` 82,933; `omp-app` 67,730; `omp-shell` 49,145; `omp-minimizer` 29,546; `omp-catalog` 27,649; `omp-agent` 24,109; `omp-core` 24,167. Small crates relevant here: `omp-adw` 2,312; `omp-kb` 1,547; `omp-proto` 1,571; `omp-oauth` 2,362; `omp-secrets` 2,189; `omp-serve` 7,616; `omp-ext` 17,093; `omp-py` 5,653; `omp-memory` 6,635; `omp-ast` 6,358; `omp-grep` 1,867. `crates/collab` has no `src` files on this commit (the script counted 0 files); driver owns `crates/driver/src/collab/`.

Roles, from `AGENTS.md` lines 8 to 73 and confirmed by the symbols in section 4: `omp-app` presents (CLI, TUI, ACP, RPC); `omp-driver` composes (`compose_kernel`); `omp-agent` owns the `Kernel`, dispatcher and job board; `omp-session`, `omp-journal`, `omp-dom` own durable session state; `omp-ai` and `omp-catalog` own inference; `omp-envd` is the project-environment host; `omp-ext` is extension configuration and trust; `omp-serve` projects gRPC services.

## 4. Turn flow, verified by symbol

| Stage | Symbol | Citation |
|---|---|---|
| Production composition | `compose_kernel` | `crates/driver/src/headless/kernel.rs:2457` |
| Agent kernel | `Kernel<C>` | `crates/agent/src/loop.rs:463` |
| Inference capability the kernel calls | `trait Inference` | `crates/agent/src/loop.rs:115` |
| Adapter from kernel to the inference gateway | `GatewayInference` | `crates/driver/src/headless/gateway.rs:30` |
| Typed tool batches | `Dispatcher` | `crates/agent/src/dispatch.rs:1236` |
| Runtime supervision index | `JobBoard` | `crates/agent/src/jobs.rs:699` |
| Canonical streamed event | `enum ChatEvent` | `crates/ai/src/event.rs:178` |
| Catalog snapshot | `Catalog` | `crates/catalog/src/snapshot.rs:58` |

Status of this path: **Implemented**. The turn path matches the prose in `AGENTS.md` ("Turn flow").

## 5. Provider runtime (`omp-ai`, `omp-catalog`, `omp-serve`)

### 5.1 One fixed Tower stack per route

`crates/ai/src/layer/stack.rs:236-262` documents the order, outer to inner: Intent, Session, Answer, Semantic, Recovery, Attempt(Admission, AccountPool, AuthLease, TransportRetry, Rate, Encode, CredentialApply, WireTransport). The composition is in `crates/ai/src/layer/stack.rs:261-274` (`build_route_stack`). Around the whole preplanned fallback service sits `build_execution_stack` (`crates/ai/src/layer/stack.rs:287`): Observe, OverallBudget, Staging. Status: **Implemented**.

```
Observe > OverallBudget > Staging > [registry fallback dispatch]
  per route: ProviderError > Operation > Intent > Session > Answer > Semantic > Recovery
             > Attempt( Admission > AccountPool > AuthLease > TransportRetry > Rate
                        > Encode > CredentialApply > WireTransport )
```

Layer responsibilities, from each file's module documentation:

| Layer | File and symbol | What it says it does |
|---|---|---|
| Account pool | `crates/ai/src/layer/account.rs:81` `AccountPoolLayer` | "Account affinity, eligibility, cooldown, quota, and explicit rotation handoff" |
| Admission | `crates/ai/src/layer/admission.rs` | "Route/account concurrency admission, bounded queuing, and readiness backpressure" |
| Retry | `crates/ai/src/layer/retry.rs` | "Pre-commit, replay-evidence-driven transport retry on the same route and account" |
| Rate | `crates/ai/src/layer/rate.rs:29` `RateLayer` | "Per-network-attempt rate reservation, deliberately inside transport retry" |
| Budget | `crates/ai/src/layer/budget.rs` | "Logical-execution budgets shared by all retries and semantic attempts" |
| Recovery | `crates/ai/src/recovery/` (dialect, empty, harmony, json, projection, reasoning, repetition, scanner, thinking, tools) | corrective repair of model output |

These are read from module documentation and symbol names. Their behaviour under load, and whether each is wired for every provider, is not verified here; the area audit [`evidence/omp2-provider-runtime.md`](evidence/omp2-provider-runtime.md) carries that work.

### 5.2 Route planning

`crates/ai/src/router.rs:148` `Router` is documented as "Capability-first, exact-selector routing and deterministic route ranking". Its imports show the planner vocabulary: `CapabilityRequirement`, `FallbackScope`, `RouteHealth`, `RuntimeRouteEvidence`, `PriceUnit`, `UsageReservePolicy`, `FallbackRevertPolicy` (`crates/ai/src/router.rs:8-45`). `crates/ai/src/registry.rs:155` `Registry` is "Immutable provider route registry", built by `RegistryBuilder` (`crates/ai/src/registry.rs:369`). The catalog keeps "provider/account domains, concrete routes, wire codecs, selectable models, and opaque wire model identifiers" distinct (`crates/catalog/src/lib.rs:1-5`), and resolution is typed (`crates/catalog/src/resolve.rs`, `selection.rs`, `cascade.rs`). Catalog data is `crates/catalog/data/catalog.postcard` plus `crates/catalog/data/sources.lock.json`. Status: **Implemented** for planning, fallback and capability negotiation by type; whether the router ranks by price or latency in practice is **not verified here**.

### 5.3 Codecs and transports present

Codec modules (`crates/ai/src/codec/`): `anthropic`, `bedrock`, `bedrock_mantle`, `connect`, `cursor`, `devin`, `gemini`, `gitlab`, `google_cca`, `native`, `ollama`, `omp_native`, `openai`, plus `openai_chat`, `openai_codex`, `openai_embedding`, `openai_media`, `openai_realtime`, `openai_responses`, and 20 `search_*` codecs (Brave, DuckDuckGo, Ecosia, Exa, Firecrawl, Google, hosted, Jina, JSON, Kagi, Mojeek, Parallel, Perplexity, scraper, SearXNG, Startpage, Tavily, TinyFish). Transport modules (`crates/ai/src/transport/`): `browser`, `capture`, `cassette`, `connect`, `custom`, `encoding`, `eventstream`, `frame`, `http`, `ndjson`, `proxy`, `sse`, `websocket`, `websocket_transport`. So OpenAI Chat, OpenAI Responses, Anthropic Messages and Gemini codecs, SSE, WebSocket, NDJSON and an AWS-style event stream transport all exist as modules. Counting the modules is a directory listing, not proof that each is reachable from a shipped route.

### 5.4 OMP2 already serves a gateway protocol

`crates/proto/proto/omp/gateway/v1/gateway.proto` defines `service Gateway` (`rpc Hello`, line 21 and 24: schema-revision and capability negotiation, with a rule that a newer client aborts when the gateway is older) and `service ForwardProxy` (line 29 and 33: a bearer-authenticated, allowlisted HTTP forward proxy where "credential selection, refresh, and header/query injection remain server-side"). `crates/proto/proto/omp/inference/v1/inference.proto:39` defines `service Inference` with `Turn` (bidirectional stream, line 48), `Fork`, `Drop`, `Realtime` (bidirectional stream, line 58), `CountTokens`, `Tokenize`, `Detokenize`, `Embed` and `GenerateImage`. `crates/proto/proto/omp/auth/v1/auth.proto:38` defines `service Auth` (list and watch credentials, begin login, submit code, wait login, put API key, put AWS credential, import OAuth, refresh). `crates/proto/proto/omp/blob/v1/blob.proto:13` defines `service Blob`. The crate that serves them is `crates/serve` (`auth.rs`, `blob.rs`, `inference.rs`, `lib.rs`, `session.rs`). The project therefore already has a gateway-shaped surface for its own clients. Whether it is exposed to third-party clients or only to OMP's own native surfaces is **not verified here**.

### 5.5 Optional heavy features

`crates/ai/Cargo.toml:16-47`: `default = []`; `local-embedding` pulls `fastembed`; `local-stt` and `local-tts` pull candle (`candle-core`, `candle-nn`, `candle-transformers`), `tokenizers`, `voice-g2p`; `realtime` pulls `webrtc`, `opus`, `audiopus_sys` and `device-attestation`. `crates/app/Cargo.toml:25-33` default features are `local`, `cap-ssh`, `cap-computer`, `cap-browser`, `cap-media`, `cap-forge`, `cap-ci`; `local-all` (applefm, embedding, stt, tts) is not default. `crates/app/Cargo.toml:94` enables `omp-ai` with `features = ["realtime"]` unconditionally, and `omp-app` imports `omp_ai::realtime::*` (`crates/app/src/chat_voice.rs:25`, `crates/app/src/live_reachability.rs:9`), so WebRTC is in the default app build. `cargo tree --offline --workspace -i webrtc` printed `webrtc v0.17.2` with `omp-ai` as its dependent. `cargo tree -i ort`, `-i fastembed` and `-i candle-core` each errored "did not match any packages", so those three are not in the default-feature graph.

## 6. Authentication and credentials

Modules present, from directory listings: `crates/ai/src/auth/` (`adc`, `alibaba_token_plan`, `apply`, `attribution`, `aws`, `broker`, `command`, `crypto`, `github_copilot`, `key`, `lease`, `login`, `manager`, `oauth`, `scope`, `search`, `shape`, `sigv4`, `spec`, `store`), `crates/ai/src/account/` (`name`, `pin`, `pool`, `quota`, `rate`, `refresh`, `store`), `crates/oauth/src` (`authorization`, `callback`, `device`, `discovery`, `metadata`, `pkce`, `registration`, `token`) and `crates/secrets/src` (`redact`, `placeholder`, `obfuscator`, `stream`, `tracked`). The stack names `AuthLeaseLayer` (`crates/ai/src/layer/stack.rs:13`) and `CredentialApplyLayer`. So API keys, OAuth with PKCE and device flow, AWS SigV4, ADC, GitHub Copilot, command-sourced credentials, encrypted storage and account refresh exist as modules; ADR 0018's STATUS row (Implemented) records the lease model. Depth (refresh coordination across processes, OS keychain use, recovery) is **not verified here**; see [`evidence/omp2-auth-credentials.md`](evidence/omp2-auth-credentials.md).

## 7. Extensions and Python (ADR 0036, re-checked)

ADR 0036 decision 5: "Every extension host and every eval session is a killable child process (0011). The engine process NEVER executes extension Python on its own thread." So the embedding is of CPython **inside the child processes**, not inside the main agent process thread. That matters for the owner's question about embedded versus subprocess Python (deliverable 8).

Re-check against `85d219ae7f`:

| ADR 0036 claim | Result |
|---|---|
| Runtime in `crates/py` (static CPython 3.14t, frozen stdlib) | `crates/py/src/lib.rs` exists. The pyo3 configs in the primary checkout (`vendor/python/pyo3-config.txt`, `vendor/python-release/pyo3-config.txt`) say `implementation=CPython`, `version=3.14`, `shared=false`, `abi3=false`, `build_flags=Py_GIL_DISABLED` (release; the debug flavour adds `Py_DEBUG`, `lib_name=python3.14td`). `/vendor` is git-ignored (`.gitignore:9`), so the interpreter is a local build product, not tracked content |
| Extension host and workers in `crates/envd/src/{exthost,worker,eval}` | `crates/envd/src/exthost/` and `crates/envd/src/eval/` exist; `worker` is no longer a directory: files `crates/envd/src/worker.rs`, `worker_child.rs`, `worker_pool.rs`, `worker_process.rs` exist. The substance was not re-verified. Path drift only |
| Signed-index extension management in `crates/ext` and `crates/app/src/ext_cli` | both exist. `crates/ext/src/` has `index.rs`, `lock.rs`, `trust.rs`, `resolver.rs`, `upgrade.rs`, `credential_grant.rs`, `doctor.rs`; Ed25519 verification is in `crates/ext/src/trust.rs` (lines 1287 to 1299 strip the `ed25519:` and `ed25519:sig:` prefixes) and `crates/ext/src/upgrade.rs:324` has `BadSignature` |
| `@remote` in `crates/py/python/omp_remote.py` and `omp/placement.py` | both exist. The STATUS gap (placement beyond the host unproved) was not re-tested |
| No JS or TS plugin runtime | `Cargo.lock` has no `deno_core`, `rquickjs`, `boa_engine`, `v8`, `rustpython`, `wasmtime`, `wasmer`, `extism`, `mlua` or `rhai` entry (grep of `^name = "..."` lines) |
| Trust tiers | `crates/ext/src/lib.rs:47` `enum TrustTier` |

ABI check: `cargo tree --offline --workspace -i libloading` printed `libloading v0.8.9` reached only through `wgpu-hal` (the GUI stack). `Cargo.lock` has no `abi_stable`, `savefile` or `stabby`. So no dynamically loaded provider or extension library exists. Source files were not searched for `extern "C"` exports; that search belongs to the extension audit ([`evidence/omp2-extensions.md`](evidence/omp2-extensions.md)).

Extensions on this commit (`extensions/`): `buildbuddy`, `ci`, `forge`, `fuzzy-index`, `github`, `gitlab`, `green-loop`, `hover-cards`, `pr-review`, `provider-pack`, `remote-approve`, `search-provider`, `sidebar`, `spec-flow`, `vault`, `welcome-chrome`, `workflows`, plus `README.md`, `RESCUE.md`, `WAVE1.md`. Go executable adapters were not located in this pass (the area audit is asked to find them or report none).

## 8. Swarm, workpool, ADW, supervision

- **There is no "swarm" crate or tool.** A search of `crates/` and `extensions/` for "swarm" (case-insensitive) matched one file: `crates/agent/prompts/modes/vibe.md:1`, which tells the model to "coordinate a broad agent swarm through the real session orchestration surface: use `task` to spawn independent workers and `hub` to message, wait for, inspect, or cancel them". So a swarm is a mode (a prompt plus Directors) over the `task` and `hub` tools.
- **Subagents are child kernels composed in the same process.** `crates/driver/src/subagent/spawn.rs` is "Journal-first child-kernel spawn composition"; `crates/tools/src/task.rs` is the "Child-agent task tool over an injected host-side spawner"; `crates/tools/src/hub.rs` is "Unified peer, job, and environment-owned named-process coordination".
- **A persistent worker pool exists.** `crates/driver/src/subagent/workpool_scheduler.rs` ("Persistent workpool scheduling over the shared job primitive") defines `WorkpoolPolicy` (line 101: `concurrency_limit`, `fresh_agents`, `eval_tools_enabled`), `WorkpoolLauncher` (line 201) and `ConWorkpoolPolicy` (line 111), whose limit comes from the convar `SV_TASK_MAX_CONCURRENCY`; `crates/driver/src/subagent/workpool_runtime.rs:31` `KernelWorkpoolLauncher` is the "Production persistent child-kernel runtime for workpool workers"; `crates/journal/src/data.rs:342` `WorkpoolMode` records `Spawned`, `Dispatched`, `Queued`, `Batch`, `Completed`. The concurrency limit is a count, not a CPU or memory budget (no resource input appears in `WorkpoolPolicy`).
- **The job board is a supervision index, not state.** `crates/agent/src/jobs.rs` documents it as "Runtime supervision index rebuilt from the authoritative `<meta><jobs>` tree" that "deliberately stores no durable job state".
- **ADW** (`crates/adw`, `Run` at `crates/adw/src/run.rs:173`, `Workflow` at `crates/adw/src/workflow.rs:167`) is a pure state machine ("it decides which phase may dispatch"); `crates/adw/src/replay.rs` rebuilds a `Run` from recorded facts (ADR 0051 R4; commit `24a034b8f6`, PR #607, 2026-11-12). The driver wires it in `crates/driver/src/adw/{definition,mod,production,remote,resume}.rs`. STATUS row 0051 still reads "Proposed 2026-11-12; not started" and "`Run` is in memory only; nothing is journaled and there is no resume", which conflicts with `resume.rs` and `replay.rs` existing. Which statement is current is **not verified here**; flagged for the swarm audit.
- **Isolation**: ADR 0007 (copy-on-write view, subagent returns a diff) is `Partially implemented` in STATUS; ADR 0071 is Withdrawn; ADR 0073 Proposed. `crates/vcs` and `crates/sandbox` exist. Not re-read here.
- **Supervision** lives in `omp-envd` (child tree: `crates/envd/src/child_tree.rs`; worker pool files in section 7) and the kernel's cancellation tree (`crates/agent/src/cancel.rs`). Not re-read here.

## 9. Protobuf and IPC

`crates/proto/proto/omp/` holds 19 `.proto` files in these packages: `auth/v1`, `blob/v1`, `collab/v1`, `control/v1`, `document/v1`, `env/v1`, `gateway/v1`, `identity/v1`, `inference/v1` (`common`, `inference`, `media`, `models`, `search`), `policy/v1`, `session/v1`, `telemetry/v1`, `thread/v1`, `toolhost/v1`, `ui/v1`. `Cargo.lock` contains `prost-build`, `protox`, `tonic-build`, `tonic-prost` and `protobuf`; ADR 0069's decision (protos compiled by one documented `protox` command, no generated gRPC clients now) is Proposed and not owner-approved. `Cargo.lock` has no `rmp-serde`, `rmp`, `capnp`, `flatbuffers` or `tantivy` entry; `rkyv` is present only as a dependency of `dtls` under `webrtc` (`cargo tree -i rkyv` printed `rkyv v0.8.18` then `dtls v0.17.2` then `webrtc v0.17.2`). `bincode` and `postcard` are present (the catalog data file is `catalog.postcard`). The isolated embedding worker speaks "bounded newline-delimited JSON frames" over stdio (`crates/memory/src/embedding/protocol.rs`). So today OMP2 uses Protobuf for native contracts, `postcard` for the catalog, and JSON lines for at least one worker. The IPC and serialization audit is [`evidence/omp2-ipc-serialization.md`](evidence/omp2-ipc-serialization.md).

## 10. Code intelligence and local inference

**Code intelligence** (all **Implemented** at the level of module existence; depth not verified here):

- `crates/kb` (`omp-kb`): "Revision-aware derived code knowledge base from SCIP producers and tree-sitter" (`crates/kb/Cargo.toml`, `description`). It runs external producers: `rust-analyzer scip` (`crates/kb/src/producer.rs:29` `.arg("scip")`), `scip-typescript` (`producer.rs:65`) and `scip-python` (`producer.rs:73`); it reads a SCIP index with `Index::read_scip` (`crates/kb/src/graph.rs:408`) and merges producer output in `crates/kb/src/store.rs:178`. It refuses to build when "repository HEAD moved while SCIP producers were running" (`crates/kb/src/error.rs:31`). It stores its cache in the target worktree's Git directory (`crates/kb/README.md`). `cargo tree -i scip` printed `scip v0.10.0` then `omp-kb` then `omp-app`. So **SCIP import already exists**. SCIP export was not found in this pass.
- `crates/ast` (tree-sitter and ast-grep: `summary.rs`, `symbol.rs`, `ops.rs`), `crates/tools/src/ast_grep.rs`, `crates/tools/src/ast_edit.rs`, `crates/tools/src/lsp/` (`navigation.rs`, `symbol.rs`, `diagnostics.rs`, `refactor.rs`), `crates/grep` (one file).
- `docs/knowledge-base/README.md` describes four tiers (text scan, AST outline, SCIP, live LSP) for the repository's own code map.
- The names `CodeQueryIR`, `CodeIR`, `ChangeIR` and `VerificationIR` from the owner's brief were not searched for by the parent; the code-intelligence audit ([`evidence/omp2-code-intelligence.md`](evidence/omp2-code-intelligence.md)) is asked to establish whether they exist in code or only in design text.

**Local inference**:

- `crates/ai/src/local/` (`applefm`, `embedding`, `parakeet`, `stt`, `tts`, `tiny_catalog`, `runtime`, `speech_catalog`, `embedding_models.json`).
- `crates/memory/src/embedding/` is an "Isolated local-embedding protocol, worker, and restartable supervisor" (`mod.rs`), with `EmbeddingSupervisor` (`crates/memory/src/embedding/supervisor.rs:50`, "Lazy, generation-fenced embedding subprocess supervision") and a stdio binary `crates/memory/src/bin/omp-memory-embedding-worker.rs`. So a lazily started, restartable local embedding worker process already exists for Mnemopi.
- Heavy runtimes are opt-in features (section 5.5). `fastembed` uses `ort` and downloads ONNX binaries (`Cargo.toml:60`, feature `ort-download-binaries-rustls-tls`), which sits uneasily with the locked rule "never C/C++ binding graphs"; this is flagged in deliverable 10.

## 11. In-flight and unmerged work

`git branch -r --no-merged origin/omp2` lists 64 remote refs (plus `upstream/main`). Relevant ones, with `git rev-list --count` and `git log` read:

| Branch | Ahead of `origin/omp2` | Content |
|---|---|---|
| `origin/feat/adw-run-replay` | 3 | ADW snapshot and replay (the squash-merged form is `24a034b8f6`); superseded in content by `crates/adw/src/replay.rs` on `omp2` |
| `origin/phase0/pyext-host-feasibility` | 1 | "test(e2e): add phase 0 extension-host feasibility instrument" (2026-11-05) |
| `origin/fix/adr0048-b9-worker-authority` | 1 | "docs(adr): close ADR 0048 B9 drift evidence" |
| `origin/feat/cleanse-routing` | 3 | routes `omp cleanse` code phases through the execution backend (2026-10-18, 1,341 commits behind) |

Branches that are not ahead by ancestry may still be squash-merged; this table is limited to what `git log origin/omp2..origin/<branch>` shows. No open pull request exists, so nothing relevant is "in review".

## 12. Drift between ADR or STATUS text and code (found while re-checking)

| # | Text | Code at `85d219ae7f` | Consequence |
|---|---|---|---|
| D1 | ADR 0036 status cites `crates/envd/src/worker` as a directory | files `crates/envd/src/worker.rs`, `worker_child.rs`, `worker_pool.rs`, `worker_process.rs` | citations in ADR text need refreshing; no design impact found |
| D2 | STATUS row 0043 cites `crates/agent/src/delegated/` | no such directory; `crates/contract/src/delegated` exists | the delegated contract moved to `omp-contract` |
| D3 | STATUS row 0051: "`Run` is in memory only; nothing is journaled and there is no resume" | `crates/adw/src/replay.rs` and `crates/driver/src/adw/resume.rs` exist | row may be stale; needs the swarm audit's verdict |
| D4 | AGENTS.md: "app -> omp-ai/realtime -> WebRTC/DTLS/Opus is a defect" when the code is never imported | `omp-app` enables `realtime` and imports `omp_ai::realtime` (`crates/app/src/chat_voice.rs:25`) | not a defect under its own wording today; the heavy graph is in the default build because voice is a default feature set |
| D5 | Locked rule: "local audio/ML via candle, never C/C++ binding graphs" | optional feature `local-embedding` pulls `fastembed` and `ort-download-binaries` (`Cargo.toml:60`) | an existing, opt-in tension, not default |
| D6 | Locked rule: "No vendor server-side tools" | ADR 0074 proposes `HostedTool::Advisor` (Proposed) | open owner decision, flagged in deliverable 20 |

## 13. What this document did not verify

- Behaviour under load of every layer in section 5.1, and whether each codec in section 5.3 is reachable from a shipped route.
- Whether `omp-serve` is reachable by third-party clients (section 5.4).
- Credential refresh coordination across processes, OS keychain use and recovery paths (section 6).
- ADR 0007, 0010, 0011, 0039, 0045, 0052, 0053, 0060, 0062 beyond their STATUS rows.
- Existence of the owner's `CodeQueryIR`, `CodeIR`, `ChangeIR`, `VerificationIR` as code.
- Anything about performance, binary size or memory: nothing is measured in this document.

## 14. Per-area audits

[Provider runtime](evidence/omp2-provider-runtime.md), [authentication](evidence/omp2-auth-credentials.md), [extensions](evidence/omp2-extensions.md), [Python runtime](evidence/omp2-python-runtime.md), [swarm and ADW](evidence/omp2-swarm-adw.md), [IPC and serialization](evidence/omp2-ipc-serialization.md), [local inference](evidence/omp2-local-inference.md), [code intelligence](evidence/omp2-code-intelligence.md). These were written by separate agents in parallel and may be unfinished at the time this document was committed; the next document, the [existing-versus-missing matrix](04-existing-vs-missing-matrix.md), is built from them.

Back to the [index](README.md).
