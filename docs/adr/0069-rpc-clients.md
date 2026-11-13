# 0069. Generated Go and Python gRPC clients: a schema bundle, an ADR 0048 amendment, then Python, then Go

Status: proposed
Date: 2026-11-13
Area: interface

Research: [docs/research/rpc-clients.md](../research/rpc-clients.md) (Gap 18 of the upstream gap closure). This record states decisions; the note holds the evidence.

## Context

### What OMPv1 does

- Typed Go, Python and Rust SDKs under `sdk/`, each an `omp-rpc` package. They target one protocol, `omp --mode rpc`, JSON lines over stdio (`upstream:sdk/go/omp-rpc/README.md:3`; framing at `upstream:packages/coding-agent/src/modes/rpc/rpc-frame.ts:5`).
- The typed surface is generated, not written: `wire.go` is marked generated (`upstream:sdk/go/omp-rpc/wire.go:1`) and regenerated with `bun run gen:rpc` (`upstream:sdk/go/omp-rpc/README.md:15-20`, `upstream:package.json:161`). The source of truth is a JSON Schema plus a TypeScript DSL (`rpc-wire.schema.json`, 132 KB). Generated files are 8,565 lines in Go, 4,395 in Python and 7,449 in Rust, around a hand-written client with request correlation, protocol-v2 negotiation and chunk reassembly.
- Upstream has no proto-generated clients. Its schema is JSON Schema, its wire is JSON.

### What omp2 has

- The proto definitions: 19 files in 15 packages under `crates/proto/proto/omp/` (`crates/proto/README.md:3`). Six gRPC services and 63 RPCs: `Auth` 21, `Blob` 4, `Gateway` 1, `ForwardProxy` 1, `Inference` 30, `SessionControl` 6 (`crates/proto/proto/omp/auth/v1/auth.proto:38`, `crates/proto/proto/omp/blob/v1/blob.proto:13`, `crates/proto/proto/omp/gateway/v1/gateway.proto:21`, `crates/proto/proto/omp/gateway/v1/gateway.proto:29`, `crates/proto/proto/omp/inference/v1/inference.proto:39`, `crates/proto/proto/omp/session/v1/session.proto:16`). The other packages are messages only; `env` and `toolhost` ride a varint-framed stdio protocol (`crates/proto/README.md:10-17`).
- The daemon registers five of the six services: `Gateway`, `ForwardProxy`, `Inference`, `Blob` and `Auth` (`crates/app/src/daemon.rs:403-408`). `SessionControl` is served separately by the session host on a Unix socket under the data directory (`crates/driver/src/session_wire.rs:318`), and `omp attach` is its client (`crates/app/src/attach_cmd.rs:33-39`). Transport on the daemon is an owner-only Unix socket, or TCP with a bearer token file; TLS and mutual TLS are supported (`crates/app/src/daemon.rs:389-391`, `crates/app/src/daemon.rs:416-418`, `crates/rpc/src/tls.rs:9-18`). Messages are capped at 64 MiB (`crates/proto/src/bounds.rs:11`). A client must perform `Gateway.Hello` itself: it sends its schema revision, the server answers with its revision and minimum (`crates/proto/proto/omp/gateway/v1/gateway.proto:43-63`); the revision is `SCHEMA_REV` 29 (`crates/proto/src/lib.rs:51`).
- Build: `build.rs` compiles with the pure-Rust `protox` and `tonic-prost-build`; generated Rust goes to `OUT_DIR` and is never committed (`crates/proto/build.rs:1-6`, `crates/proto/README.md:18`). CI runs `buf format`, `buf lint` and `buf breaking` (WIRE) against the PR base (`.github/workflows/ci.yml:203-229`). `buf.yaml` ignores `control`, `telemetry`, `ui` and `collab` for breaking checks (`crates/proto/buf.yaml`).
- `omp rpc` is not proto: serde structs with free-form `serde_json::Value` payloads and no JSON Schema (`crates/rpc/src/protocol.rs:1-6`). Its framing is the same as upstream's: newline-delimited JSON with protocol-v2 chunk reassembly, a 1 MiB line cap and a 64 MiB reassembled cap (`crates/app/src/rpc_mode.rs:34`, `crates/rpc/src/framing.rs:1-2`, `crates/rpc/src/framing.rs:13-21`). The crate README and the CLI help still say "Content-Length" (`crates/rpc/README.md:8`, `crates/app/src/cli.rs:1127`); those texts look stale. Upstream's schema lists 74 commands; a string scan of omp2's match arms finds 45 of those names handled, 29 upstream-only and 7 omp2-only (`x-rpc.commands` in `upstream:packages/coding-agent/src/modes/rpc/wire/rpc-wire.schema.json`). Field shapes were not compared (UNVERIFIED). Upstream's SDKs may therefore work against omp2 for the shared commands.
- `omp.thread.v1` imports `inference/v1/common.proto`, and `omp.inference.v1` imports `thread/v1/thread.proto`; buf's cycle rule is switched off for this (`crates/proto/proto/omp/thread/v1/thread.proto:5`, `crates/proto/proto/omp/inference/v1/inference.proto:9`, `crates/proto/buf.yaml`). No file has a `go_package` option. There is no Go code in the workspace (the one `go.mod` is a fixture).

### Rules that bind

- ADR 0048: Buf is limited to format, lint and breaking; "`buf generate`, the BSR, Bazel proto rules and BuildBuddy caching for proto generation are all rejected"; generated code is never committed (`docs/adr/0048-schema-and-serialization-boundaries.md:12`, `docs/adr/0048-schema-and-serialization-boundaries.md:27`). That text is about the Rust build; its wording would be read against Go and Python generation too.
- ADR 0048's statement that CI does not run `buf breaking` is out of date: it does (`docs/adr/0048-schema-and-serialization-boundaries.md:29`, `.github/workflows/ci.yml:219-229`).
- No new language runtimes. A Go or Python client is a separate artifact, not a runtime inside omp, but a Go toolchain in CI is new to this repository.
- Additive only: no existing proto, service, convar or command changes.

## Decision

**D1. Publish the schema first.** A release artifact: the `.proto` sources, `buf.yaml`, a `FileDescriptorSet` compiled by the `protox` the build already uses, and a manifest with `SCHEMA_REV` and the git revision. No new generator. Anyone can generate any language from it.

**D2. Amend ADR 0048 before any generation.** The owner rules on client generation: which tool (`buf generate` with local plugins, or `protoc`), that output is never committed, where artifacts are published, and the pinned versions and licence check. The amendment also fixes the stale `buf breaking` sentence.

**D3. Python first, Go second.** Clients are built in a release job and published as wheels and a Go module; generated code is never committed. A thin hand-written layer on each: a Unix-socket channel helper, TLS and bearer-token credentials, the `Hello` handshake that refuses a server whose minimum revision exceeds the client's, and receive limits raised to 64 MiB. Scope: the five daemon services; `SessionControl` is a second step because its server is the session host on another socket.

**D4. Go needs the cycle solved without touching the schema.** Map `thread` and `inference` to one Go import path in a generation-only configuration. Whether protoc-gen-go accepts two proto packages in one Go package is UNVERIFIED; if not, breaking the cycle in the schema is a breaking proto change and gets its own decision.

**D5. One conformance scenario list.** Hello, unary, server stream, client stream, large message and schema mismatch, run by every client against one daemon. Clients carry the `SCHEMA_REV` they were generated at and a release fails if they disagree with the bundle.

**D6. Names and gates.** Packages are `omp-grpc` (Python) and an `omp` Go module; no `pi` identity. CI jobs are opt-in (`workflow_dispatch` first), so nothing runs unless asked.

**D8. Probe upstream's SDKs against `omp rpc` before building anything for the stdio protocol.** The framing is shared and 45 of 74 command names overlap, so upstream's MIT Go, Python and Rust `omp-rpc` packages may already work for the shared commands. If the probe passes, adopt them for that protocol, with the differences listed; no schema work is needed for that path.

## Rejected options

- **Do nothing.** Users can run `protoc` or `grpcurl` already, but nobody can find the contract or its version.
- **Check generated clients into the repository.** Rejected by ADR 0048.
- **Port upstream's JSON-schema generator for `omp rpc`.** It first needs a schema for a protocol whose payloads are free-form `Value`; very high cost. Superseded for the common case by D8.
- **Generate stdio clients from Rust types with `schemars`.** High cost and the open payloads stay untyped.
- **Edit the `.proto` files to add `go_package`.** It changes shared sources for a downstream need; use generation-time mapping.

## Slices

Each slice is one PR; tests are written first and not edited to pass.

**S0. Compatibility probe (no product code).** Run upstream's Python and Go `omp-rpc` suites and a scripted session against `omp rpc`.
- Acceptance: a table of the 74 upstream commands with pass, fail or missing and the failing field; a yes or no on adopting the SDKs; the stale `Content-Length` text in `crates/rpc/README.md:8` and `crates/app/src/cli.rs:1127` fixed or flagged.

**S1. Schema bundle.** A `just` target or a bin in `crates/proto`.
- Tests: `bundle_contains_the_19_proto_files`, `descriptor_set_lists_six_services_and_63_rpcs` (adding a service forces a deliberate update), `manifest_revision_equals_schema_rev`, `two_builds_are_byte_identical`.
- Gate: a `just` target; not in the default build.

**S2. ADR 0048 amendment (prerequisite for S3 and S4).** A documentation PR.
- Acceptance: the amendment names the generator, its pinned version and the licence check; CI already pins buf 1.73.0 (`.github/workflows/ci.yml:170-208`); the stale sentence is fixed.

**S3. Python client `omp-grpc`.**
- Tests, against a live daemon on a socket: `hello_returns_the_three_capabilities`, `unary_auth_call_succeeds`, `inference_server_stream_delivers_chunks`, `blob_upload_client_stream_round_trips`, `client_older_than_min_schema_rev_fails_with_a_typed_error`, `message_near_64_mib_is_accepted`.
- Gate: opt-in CI job.

**S4. Go client.**
- Tests: `go build` and `go vet` pass on the generated module; the same conformance list passes in Go; `thread_and_inference_share_one_go_package`.
- Gate: opt-in CI job; Go installed only there.

**S5. Conformance and versioning.** One scenario list, every client passes it; a release fails when a client's revision and the bundle disagree.

## Upstream origin and license

If the S0 probe passes, upstream's SDKs (MIT) are reused for the stdio `omp rpc` protocol; otherwise nothing is reused. Ideas worth keeping either way are the binding rules (strict enums, an `Extra` map for open records) and the split between a generated wire file and a hand-written client (`upstream:sdk/go/omp-rpc/README.md:22-29`). Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`); its Python SDK carries its own MIT file (`upstream:sdk/python/omp-rpc/LICENSE:1-3`). Generator tools (buf, protoc-gen-go, grpc-go, grpcio-tools) have their own licences, not checked here (UNVERIFIED); `deny.toml` covers Rust crates only, so the review is manual.

## Consequences

- Outside users get a versioned, language-neutral contract, then typed clients.
- A published client is a compatibility promise for the services it covers. `buf breaking` already checks WIRE, but four packages are ignored.
- The stdio `omp rpc` protocol stays hand-written and untyped in its payloads.

## Open questions for the owner

- **O1.** Accept `buf generate` or `protoc` for non-Rust clients as a release step? Recommendation: yes, with output never committed, buf pinned as in CI. Without a yes, S3 and S4 do not start.
- **O2.** Go toolchain in this repository's CI, or a separate repository for Go? Recommendation: an opt-in CI job here; move it out if Go grows beyond generation.
- **O3.** Does one Go package for both cyclic proto packages generate cleanly? Not tried. Recommendation: try it in S4 before any schema change.
- **O4.** Scope: five daemon services, or also `SessionControl`? Recommendation: the five first; add `SessionControl` once its owner-socket authority model is traced.
- **O5.** Is gRPC the supported programmatic surface, or is a typed `omp rpc` client wanted? Recommendation: both, in this order: the S0 probe decides whether upstream's MIT SDKs serve the stdio protocol; gRPC clients follow the amendment.
- **O6.** Which services are stable enough to promise? Recommendation: `Gateway` and `Auth` first; mark `Inference` experimental until the owner says otherwise.
- **O7.** Server-side message cap for remote TCP. Recommendation: evaluate a lower cap before publishing a remote client.

## Status in omp

**Status: Not started.** Reusable today: the proto sources and `buf.yaml`, `protox` in `build.rs`, the buf pin in CI, `Gateway.Hello`, the UDS and TLS plumbing.

## References

[docs/research/rpc-clients.md](../research/rpc-clients.md); ADR 0048 (schema and serialization boundaries); `docs/research/protocols-schemas-serialization.md`.

