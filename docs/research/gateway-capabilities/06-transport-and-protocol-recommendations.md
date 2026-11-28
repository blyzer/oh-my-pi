# Transport and protocol recommendations

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 6. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Part A (this file) covers inference transports and protocol translation. Part B covers IPC and serialization formats and lives in [`06b-ipc-and-serialization.md`](06b-ipc-and-serialization.md). Evidence: [`evidence/omp2-provider-runtime.md`](evidence/omp2-provider-runtime.md) sections 3 and 4, [`evidence/measurements/11-catalog-census.md`](evidence/measurements/11-catalog-census.md). Provider-runtime cards `PRV-01` to `PRV-09` are in [`05-provider-integration-recommendations.md`](05-provider-integration-recommendations.md).

## Part A

### A1. Transports present, counted from the catalog

| Transport | Routes in snapshot | Implementation | Maturity |
|---|---|---|---|
| HTTP/1.1 and HTTP/2 over rustls | 165 of 171 | pooled hyper client, `crates/ai/src/transport/http.rs:318,336` | Implemented |
| SSE and NDJSON framing | (framing on HTTP routes) | `transport/sse.rs`, `transport/ndjson.rs` | Implemented |
| WebSocket | 1 | `transport/websocket_transport.rs`; two providers set `codex_transport = "websocket-preferred"` | Implemented |
| AWS EventStream | 2 | `transport/eventstream.rs` | Implemented |
| Connect (Cursor) | 2 | `transport/connect.rs`, `codec/cursor.rs` | Implemented |
| Local in-process | 1 | `crates/ai/src/local` | Implemented |
| WebRTC | 0 | `crates/ai/src/realtime` behind feature `realtime`; `omp-app` enables it (`crates/app/Cargo.toml:94`) | Implemented, not a catalog route |
| gRPC | not a provider transport | served by `omp-serve` via tonic (`inference.proto:39`) | Partial |
| HTTP/3, QUIC | 0 | no code; `quinn`, `h3` not in the resolved graph | Absent |

Rule from the owner brief section 5: do not add a transport without a demonstrated provider use case. The audit finds no provider that needs HTTP/3 or gRPC, and exactly one place (voice) where WebRTC is used.

### A2. Protocol translation architecture

OMP2 uses a canonical intermediate representation: one sans-I/O `Codec` per wire protocol lowers a canonical `OperationCall` and decodes frames to `RawEvent` and `ChatEvent` (`crates/ai/src/codec/mod.rs:918,797`, `crates/ai/src/event.rs:178`). The snapshot has 33 codec ids: 14 model-serving (`openai-chat`, `anthropic`, `openai-responses`, `openai-codex`, `google-cca`, `google-genai`, `google-vertex`, `bedrock-converse`, `bedrock-mantle`, `cursor`, `devin`, `gitlab-duo`, `ollama`, `local`), one `parallel-extract`, 18 web-search codecs. Adding a provider that speaks OpenAI Chat is data only. This matches the Locked Deviation "provider stream frames decode to canonical semantic events, never forwarded literally" and ADR 0022. Pairwise translators (A to B wire) do not exist and are not proposed: N codecs against one IR is N units of work, pairwise is N squared.

## Cards

### PRV-10 HTTP/3 and QUIC
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** `cargo tree --offline --locked --workspace -i quinn` and `-i h3` return "did not match any packages"; `Cargo.lock` lists `quinn` only as a dependency of `reqwest`, which is not on the compiled graph for this purpose. `crates/ai/src/transport/http.rs:318` enables HTTP/2 only. Maturity: Absent.
**Actual missing capability:** None demonstrated. No provider in the 125-provider catalog is documented here as requiring HTTP/3.
**Alternatives considered:** Adopt `quinn` and `h3` behind a feature; rejected without a provider need.
**Expected benefit:** Possible connection-setup latency on lossy links, not measured and not shown to matter for streaming chat.
**Implementation complexity:** high (UDP, certificate and proxy behaviour, a new stack beside hyper).
**Security implications:** New network stack and proxy-bypass surface (QUIC does not pass HTTP proxies); the existing `ProxyDiscoveryRoutes` and proxy transport (`transport/proxy.rs`) would not cover it.
**Maintenance cost:** High.
**Performance measurement strategy:** Not applicable.
**Acceptance criteria:** Revisit only if a catalog provider documents HTTP/3 as required or materially faster.
**Follow-up:** None.

### PRV-11 WebRTC feature weight in the default app build
**Decision class:** Requires benchmarking
**Existing implementation evidence:** `webrtc` is optional in `omp-ai` and enabled only by `realtime` (`crates/ai/Cargo.toml:40-46`); `omp-app` depends on `omp-ai` with `features = ["realtime"]` unconditionally (`crates/app/Cargo.toml:94`) and imports it (`crates/app/src/chat_voice.rs:25`, `chat_control.rs:25`, `live_reachability.rs:9`); `omp-ai` is the only direct dependent of `webrtc` in `Cargo.lock`. The AGENTS.md example of an unused `app -> omp-ai/realtime -> WebRTC` edge is therefore stale: the edge is used. No catalog route has transport `Webrtc`. Maturity: Implemented.
**Actual missing capability:** A measured answer to "does the voice path earn its build and size weight" under the AGENTS.md gate that cold `cargo run --bin omp` build time must be justified.
**Alternatives considered:** Gate behind an `omp-app` feature `cap-voice` (default on, consistent with ADR 0049 D3); move realtime to an out-of-process helper; leave.
**Expected benefit:** Build time and binary size only; no inference benefit.
**Implementation complexity:** low for a feature gate, medium for a helper.
**Security implications:** Smaller default attack surface if voice is optional; the device-attestation feature is already coupled to `realtime` (`Cargo.toml:40-46`).
**Maintenance cost:** Low.
**Performance measurement strategy:** `cargo build --timings` cold for `omp` with and without `omp-ai/realtime`, crate count from `cargo tree`, stripped binary size. Not measured here.
**Acceptance criteria:** Measurements recorded under `evidence/measurements/`; owner decides keep, feature-gate or remove.
**Follow-up:** Gate if the delta is material.

### PRV-12 gRPC as a provider transport
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** OMP2 serves gRPC (`service Inference`, `crates/proto/proto/omp/inference/v1/inference.proto:39`, bidirectional `Turn` at `:48`, `Realtime` at `:58`; `crates/serve/Cargo.toml:33`). No provider codec uses gRPC (`grep -il grpc crates/ai/src` returns only `codec/cursor.rs` and `codec/omp_native.rs`). `tonic` is used by omp-app, omp-driver, omp-e2e, omp-envd, omp-exec-reapi, omp-proto, omp-rpc, omp-serve, not by `omp-ai` (`Cargo.lock`). Maturity: Partial (served only).
**Actual missing capability:** None.
**Alternatives considered:** A gRPC codec for a provider that offers it; none demonstrated.
**Expected benefit:** None shown.
**Implementation complexity:** medium.
**Security implications:** None new.
**Maintenance cost:** Medium.
**Performance measurement strategy:** Not applicable.
**Acceptance criteria:** Revisit if a catalog provider exposes only gRPC.
**Follow-up:** None.

### PRV-13 OpenAI-compatible HTTP facade over `omp-serve`
**Decision class:** Suitable as an optional extension
**Existing implementation evidence:** `omp-serve` projects the typed registry as a tonic `Inference` service (`crates/serve/src/inference.rs`); a grep for `chat/completions` in `crates/serve/src`, `crates/driver/src`, `crates/app/src` finds nothing. Maturity: Absent.
**Actual missing capability:** Letting a third-party tool that only speaks OpenAI-compatible HTTP use OMP2's routing, auth and accounting.
**Alternatives considered:** Run an external gateway (see the reference comparison) beside OMP2; build the facade in core (rejected: owner brief says OMP2 is not another gateway).
**Expected benefit:** Interoperability only; no inference or token saving.
**Implementation complexity:** medium: request and stream mapping, auth for the listener.
**Security implications:** Opens a network listener that spends the user's credentials; must bind loopback by default and require a token, like the existing serve auth (`crates/serve/src/auth.rs`).
**Maintenance cost:** Medium.
**Performance measurement strategy:** Added latency of the facade over the gRPC path, per-token overhead; not measured.
**Acceptance criteria:** An extension (T2) maps `/v1/chat/completions` to the `Inference` service with streaming, with no change to `omp-ai`.
**Follow-up:** Only on a concrete consumer.

### PRV-14 Keep canonical IR; no pairwise translators
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `Codec` and `Decoder` traits and `RawEvent` (`crates/ai/src/codec/mod.rs:918,844,797`), `ChatEvent` (`crates/ai/src/event.rs:178`), 33 codec ids (measurement 11), ADR 0022 Implemented (`docs/adr/STATUS.md:40`). Maturity: Implemented.
**Actual missing capability:** None.
**Alternatives considered:** Pairwise translators; rejected (quadratic, and violates canonical-event rule).
**Expected benefit:** Maintenance.
**Implementation complexity:** not applicable.
**Security implications:** Typed events keep tool execution authorization on `ToolCallReady` only (`event.rs:218`).
**Maintenance cost:** Low.
**Performance measurement strategy:** Token streaming overhead per codec is not measured; plan: replay a cassette through each decoder and time frames per second.
**Acceptance criteria:** New providers on an existing wire protocol are catalog data only.
**Follow-up:** None.

### PRV-15 Failure classification tables as catalog data
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** Deterministic classifier in Rust: `classify_http_error_with_hint` (`crates/ai/src/transport/http.rs:1273`), context-overflow phrase list (`crates/ai/src/error.rs:156-178`), per-codec classifiers (`anthropic.rs:3326`, `openai_chat.rs:2595`, `openai_responses.rs:3764`). Locked Deviation: providers-as-data. Maturity: Partial.
**Actual missing capability:** Account-cap and overflow phrases per provider live in Rust string tables; they could be catalog data with fixture tests.
**Alternatives considered:** Leave in Rust (works, deterministic).
**Expected benefit:** Reliability and maintainability: a new provider phrase is a data change.
**Implementation complexity:** medium.
**Security implications:** None.
**Maintenance cost:** Lower than now.
**Performance measurement strategy:** Not applicable.
**Acceptance criteria:** Existing classification tests pass unchanged against data-driven tables.
**Follow-up:** Low priority.

### PRV-16 Queue-latency and per-route metrics
**Decision class:** Missing and justified: propose
**Existing implementation evidence:** Metrics for tokens, calls, duration, errors, cost (`crates/observability/src/metrics.rs:73-113`); TTFT as span attribute (`crates/observability/src/span.rs:123,430`); admission queue exists (`crates/ai/src/layer/admission.rs:24`) but no queue-wait metric was found. Maturity: Partial.
**Actual missing capability:** Admission queue wait, per-route error and retry counters, fallback counts, as OTel metrics.
**Alternatives considered:** Derive from spans offline.
**Expected benefit:** Latency visibility; input for PRV-01 and PRV-03 evidence.
**Implementation complexity:** low.
**Security implications:** Keep route ids, never account ids, in metric labels.
**Maintenance cost:** Low.
**Performance measurement strategy:** Overhead of one counter increment per attempt, not measured.
**Acceptance criteria:** Metrics appear in the existing exporter with route and outcome labels.
**Follow-up:** None.

### PRV-17 End-to-end streaming backpressure on HTTP
**Decision class:** Requires benchmarking
**Existing implementation evidence:** Bounded realtime queue with a typed `Backpressure` error (`crates/ai/src/operation/realtime.rs:169`), `ErrorKind::PolicyBufferExceeded` and a `provisional_bytes` counter (`crates/ai/src/layer/mod.rs:93`). Whether a slow consumer stalls the HTTP body read (hyper poll) rather than buffering unboundedly was not verified. Maturity: Partial.
**Actual missing capability:** A measured guarantee that a slow UI cannot cause unbounded memory growth on a long stream.
**Alternatives considered:** Assume hyper's pull model suffices.
**Expected benefit:** Memory safety under slow consumers.
**Implementation complexity:** low if only tests are needed.
**Security implications:** Unbounded buffering is a denial-of-service on the local process.
**Maintenance cost:** Low.
**Performance measurement strategy:** Fake streaming server with a stalled reader; record RSS growth over N MB streamed. Not measured.
**Acceptance criteria:** RSS stays under a stated bound while the consumer is paused.
**Follow-up:** Extend limits if the test fails.

## Part B

IPC and serialization formats (Protobuf, JSON-RPC, shared memory and the rest) are assessed in [`06b-ipc-and-serialization.md`](06b-ipc-and-serialization.md).

Back to the [index](README.md).
