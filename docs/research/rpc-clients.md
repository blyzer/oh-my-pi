# Gap 18: generated Go and Python RPC clients from the proto definitions

Status: research only. Nothing here is implemented or authorized.
Date: 2026-11-13
Baseline: omp2 f4186b783b; upstream c308970263 (pinned checkout).

Evidence keys:

- **OBSERVED** means read in code or docs at the baseline. Citations use `path:line`. Upstream paths carry the `upstream:` prefix.
- **[INFERENCE]** means the author's reasoning. Every size estimate is an inference.
- **UNVERIFIED** means no run or source confirmed it.

No build, test or run was made for this document. No generator was run.

## Question

Upstream ships typed Go, Python and Rust clients for its RPC protocol. Can omp2 generate Go and Python clients from the proto definitions it already owns, and what in omp2's own decisions stands in the way?

Out of scope: a Rust client (omp2 already has one, `omp-rpc`), and clients for ACP, MCP or the collab relay.

## 1. Upstream

- Three SDKs live under `sdk/`: `go`, `python` and `rust`, each an `omp-rpc` package (directory listing of `upstream:sdk`).
- They target one protocol: `omp --mode rpc`, JSON lines over stdio (`upstream:sdk/go/omp-rpc/README.md:3`). The TypeScript side frames each message as one newline-delimited UTF-8 JSON object with a size cap (`upstream:packages/coding-agent/src/modes/rpc/rpc-frame.ts:5`).
- The typed surface is generated, not written. `wire.go` is marked "generated": every frame and shared type, JSON codecs, one parameter struct and one typed method per command (`upstream:sdk/go/omp-rpc/README.md:8-9`; first line `upstream:sdk/go/omp-rpc/wire.go:1`). Regeneration is `bun run gen:rpc` (`upstream:sdk/go/omp-rpc/README.md:15-20`), which runs `bun scripts/gen-rpc/index.ts` (`upstream:package.json:161`, `upstream:packages/coding-agent/package.json:529`).
- The source of truth is a JSON Schema plus a TypeScript DSL under `packages/coding-agent/src/modes/rpc/wire` (`rpc-wire.schema.json`, 132 KB; `rpc-wire.generated.ts`, 53 KB). A conformance test type-checks the schema against the hand-written types (`upstream:packages/coding-agent/test/rpc-wire/conformance.types.ts`).
- Sizes: the generated Go file is 8,565 lines, the Python `_wire.py` 4,395 lines and the Rust `wire.rs` 7,449 lines, counted with `wc -l`. Around them sits hand-written code: client with request correlation and protocol-v2 negotiation, chunk reassembly, host tools and host URI schemes (`upstream:sdk/go/omp-rpc/README.md:10-13`).
- Binding rules the generator fixes: enums are string types and decoding an unknown value fails; closed objects decode strictly; open records keep undeclared keys in an `Extra` map and write them back (`upstream:sdk/go/omp-rpc/README.md:24-29`).
- The Go module is `github.com/can1357/oh-my-pi/sdk/go/omp-rpc`, Go 1.23, standard library only (`upstream:sdk/go/omp-rpc/go.mod:1-3`).

What upstream does **not** have: proto-generated clients. Its protocol is JSON, and its schema is JSON Schema.

## 2. omp2 inventory

### The proto definitions

- `omp-proto` "owns the workspace's wire-level Protobuf definitions and the Rust bindings generated from them"; the optional `tonic` feature adds gRPC clients and servers (`crates/proto/README.md:3`). Sources are `crates/proto/proto/omp/<package>/v1/*.proto`.
- Inventory, counted with `find` and `grep` on the baseline:
  - 19 files in 15 packages: `auth`, `blob`, `collab`, `control`, `document`, `env`, `gateway`, `identity`, `inference` (five files, one package), `policy`, `session`, `telemetry`, `thread`, `toolhost`, `ui`.
  - 6 gRPC services and 63 RPCs: `Auth` (21 RPCs, 20 unary and 1 server stream), `Blob` (4: 2 unary, 1 server stream, 1 client stream), `Gateway` (1 unary, `Hello`), `ForwardProxy` (1 server stream, `Forward`), `Inference` (30: 23 unary, 5 server stream, 2 bidirectional) and `SessionControl` (6: 5 unary, 1 server stream). Service declarations: `crates/proto/proto/omp/auth/v1/auth.proto:38`, `crates/proto/proto/omp/blob/v1/blob.proto:13`, `crates/proto/proto/omp/gateway/v1/gateway.proto:21`, `crates/proto/proto/omp/gateway/v1/gateway.proto:29`, `crates/proto/proto/omp/inference/v1/inference.proto:39`, `crates/proto/proto/omp/session/v1/session.proto:16`.
  - The other packages (`env`, `toolhost`, `control`, `ui`, `telemetry`, `document`, `collab` and so on) define messages only. `env` and `toolhost` ride a varint-framed stdio protocol, not gRPC (`crates/proto/README.md:10-17`). Their wire is not a client surface.
- Services actually served: the daemon registers five of the six, `Gateway`, `ForwardProxy`, `Inference`, `Blob` and `Auth` (`crates/app/src/daemon.rs:403-408`). `SessionControl` is served separately, by the session host on a Unix socket under the data directory, wrapped in `SessionControlRpc` (`crates/driver/src/session_wire.rs:318`); `omp attach` is its client (`crates/app/src/attach_cmd.rs:33-39`). An earlier draft of this note said it was unserved; that was wrong, because the search used a wrong type name. Its authority model, scopes and socket lifetime were not traced (UNVERIFIED).
- Hello advertises capabilities `auth`, `inference.native` and `gateway.forward` (`crates/app/src/daemon.rs:380-386`). `HelloRequest` carries the client name, its schema revision and capabilities; `HelloResponse` carries the server's revision, its minimum revision, capabilities and version (`crates/proto/proto/omp/gateway/v1/gateway.proto:43-63`). The wire-visible revision is `SCHEMA_REV`, 29 (`crates/proto/src/lib.rs:51`). A client has to perform this handshake itself.
- Transports: an owner-only Unix-domain socket locally, or TCP with a bearer token file; plain TCP without a token is refused (`crates/app/src/daemon.rs:389-391`, `crates/app/src/daemon.rs:416-418`, `crates/app/src/daemon.rs:177`). TLS and optional mutual TLS are supported by the `omp-rpc` crate (`crates/rpc/src/tls.rs:9-18`). Server messages are capped at 64 MiB (`crates/proto/src/bounds.rs:11`, `crates/app/src/daemon.rs:63`), far above the default receive limits of grpc-go and grpcio, so a client must raise its limits. [INFERENCE: those defaults are from general knowledge, not read here.]
- Build and checks: `build.rs` compiles every `.proto` with the pure-Rust `protox` compiler and `tonic-prost-build`; generated Rust goes to `OUT_DIR` and is never committed (`crates/proto/build.rs:1-6`, `crates/proto/README.md:18`). `buf.yaml` sets lint to BASIC and breaking to WIRE, and ignores `control`, `telemetry`, `ui` and `collab` (`crates/proto/buf.yaml`). CI runs `buf format`, `buf lint` and `buf breaking` against the PR base (`.github/workflows/ci.yml:203-229`). ADR 0048's text says CI does not run `buf breaking`; the workflow now does, so that sentence is out of date (`docs/adr/0048-schema-and-serialization-boundaries.md:29`).
- Package cycle: `omp.thread.v1` imports `omp/inference/v1/common.proto`, and `omp.inference.v1` imports `omp/thread/v1/thread.proto` (`crates/proto/proto/omp/thread/v1/thread.proto:5`, `crates/proto/proto/omp/inference/v1/inference.proto:9`, `crates/proto/proto/omp/inference/v1/media.proto:6`). Buf's `PACKAGE_NO_IMPORT_CYCLE` rule is switched off for exactly this reason (`crates/proto/buf.yaml`). No `go_package` option appears in any of these files (searched: `option` lines in `crates/proto/proto/omp`).

### The `omp rpc` embedding protocol is not proto

- `omp rpc` is the stdio embedding protocol. Its types are serde structs that "intentionally leave application-owned model, message, and event payloads as `serde_json::Value`" (`crates/rpc/src/protocol.rs:1-6`). There is no JSON Schema for it: `JsonSchema` appears zero times in `protocol.rs`. The Rust client is the `client` module of `omp-rpc` (`crates/rpc/README.md:7`).
- **The framing is the same as upstream's.** The stdio mode decodes newline-delimited JSON with protocol-v2 chunk reassembly (`JsonLineDecoder`, `RpcFrameDecoder`), with a 1 MiB cap per physical line and 64 MiB per reassembled payload (`crates/app/src/rpc_mode.rs:34`, `crates/rpc/src/framing.rs:1-2`, `crates/rpc/src/framing.rs:13-21`). The crate README and the CLI help say "Content-Length" (`crates/rpc/README.md:8`, `crates/app/src/cli.rs:1127`); `rpc_mode.rs` imports the JSON-line decoder, not the Content-Length one (`crates/rpc/src/framing.rs:159`). Those two texts look stale. An earlier draft of this note said the framing differed; that was wrong.
- **Command overlap.** Upstream's schema lists 74 commands (`x-rpc.commands` in `upstream:packages/coding-agent/src/modes/rpc/wire/rpc-wire.schema.json`). A scan of the match arms in `crates/app/src/rpc_mode.rs` finds 45 of those names handled by omp2, 29 upstream-only (for example `fork`, `goal`, `btw`, `live_start`, `dictation_start`, `navigate_tree`, `logout`) and 7 names only omp2 has (`approve`, `cancel`, `resume`, `shutdown` and three frame names). The scan is a string heuristic. Field shapes were not compared (UNVERIFIED). Upstream's SDKs may therefore work against omp2 for the shared commands.

### Locked decisions

- ADR 0048 keeps Buf to format, lint and breaking. "`buf generate`, the BSR, Bazel proto rules and BuildBuddy caching for proto generation are all rejected" (`docs/adr/0048-schema-and-serialization-boundaries.md:27`). Hybrid D, Buf for checks and Cargo for codegen, is "what OMP² does today" (`docs/research/protocols-schemas-serialization.md:263`). That reasoning concerns the Rust build, but its wording would be read against `buf generate` for Go and Python too.
- Generated code is never committed, and there is no system `protoc` (`docs/adr/0048-schema-and-serialization-boundaries.md:12`).
- Hard constraint: no new language runtimes. A Go or Python client is a separate artifact for outside users, not a runtime inside omp, but a Go toolchain in CI is still new to this repository. The owner should say whether that counts.
- A repository search for a `go.mod` found only a fixture (`fixtures/sarif/sources/golangci/go.mod`). There is no Go code in the workspace today.

### Gap table

| Item | Upstream | omp2 | Gap |
|---|---|---|---|
| Typed Go client | generated from JSON Schema | none | new |
| Typed Python client | generated from JSON Schema | none | new |
| Machine-readable schema | JSON Schema + DSL | `.proto` for gRPC; none for `omp rpc` | gRPC side exists |
| Handshake and version check | protocol v2 negotiation | `Gateway.Hello` with `schema_rev` and `min_schema_rev` | client must implement |
| Compatibility check in CI | schema conformance test | `buf breaking` (WIRE) | exists |
| Generation tool | `bun run gen:rpc` | none for non-Rust; ADR 0048 rejects `buf generate` | needs a ruling |
| Go packaging | module in the repo | package cycle thread/inference; no `go_package` | design needed |
| Services to expose | the whole stdio surface | 6 services: 5 on the daemon, `SessionControl` on the session host | choose scope |

## 3. Class

**Class d (propose)** for proto-generated gRPC clients. omp2 has proto definitions and a served gRPC surface, which is a good generation source, but no generator, no Go code and a locked ADR against `buf generate`. For the stdio `omp rpc` protocol there is a second, cheaper path that may be **class c**: upstream's MIT SDKs speak newline-delimited JSON, and omp2 uses the same framing and shares 45 of 74 command names, so they may work with little change. That needs a compatibility probe first (option 7).

Verdict: do not generate gRPC clients yet. First ship the schema as a versioned bundle (cheap, no new tool, no ADR conflict), then ask for the ADR 0048 amendment before any Go or Python generation. In parallel, run the compatibility probe for upstream's SDKs against `omp rpc`; if it passes, adopting them is the cheapest way to give Go and Python users a typed client.

## 4. Options

1. **Do nothing.** Anyone can run `protoc`, `buf` or `grpcurl` on `crates/proto/proto`. Cost: zero. Fit: honest, but nobody can find the contract or its version.
2. **Publish the proto sources and a descriptor set as a release artifact** ("protos as the contract"). Cost: 1 to 2 days. Fit: good. It needs no new generator; the `protox` compiler the build already uses can emit a `FileDescriptorSet`. Consumers generate for any language with their own tools.
3. **Generate Python and Go with `buf generate` or `protoc` plugins in a release job, publishing wheels and a Go module, never committing generated code.** Cost: 2 to 3 weeks for both. Fit: good for users, but it needs an ADR 0048 amendment and a Go toolchain in CI.
4. **Check generated clients into the repository.** Rejected: generated code is never committed (`docs/adr/0048-schema-and-serialization-boundaries.md:12`).
5. **Port upstream's JSON-schema generator to `omp rpc`.** Cost: very high. It first needs a schema for a protocol whose payloads are free-form `Value`. Fit: poor. Superseded for the common case by option 7.
6. **Generate clients for the stdio protocol from Rust types** with `schemars`. Cost: high, and the open payloads stay untyped. Fit: poor.
7. **Reuse upstream's SDKs for `omp rpc`.** Take the Go, Python and Rust `omp-rpc` packages (MIT) and run them against omp2's stdio mode, fixing the differences. Cost: a probe of 1 to 2 days, then an unknown amount of adaptation for the 29 upstream-only and 7 omp2-only commands, whose wire shapes differ or are missing. Fit: good if the probe passes, because the framing is shared and the typed surface already exists. Risks: the generated `wire.go` and `_wire.py` encode upstream's schema, so omp2 changes need a regenerated or hand-patched wire file, and the generator is a Bun script (`upstream:package.json:161`), a runtime omp2 does not use.

Choice: option 2 now, then option 3 for Python first and Go second, subject to the ADR amendment; option 7's probe in parallel, and if it passes, option 7 for the stdio protocol. [INFERENCE]

## 5. Plan

None of these slices changes an existing proto, service, convar or command. Each ships as a build or release artifact, off unless invoked.

### Slice 0: compatibility probe (no repository change)

- Run upstream's Python and Go `omp-rpc` test suites and a short scripted session against `omp rpc`, and record for each of the 74 upstream commands whether it passes, fails or is missing.
- Acceptance: a table of all 74 commands with a result and the failing field where there is one, and a yes or no on adopting the SDKs for the shared commands. Also fix or flag the stale "Content-Length" text in `crates/rpc/README.md:8` and `crates/app/src/cli.rs:1127`.

### Slice 1: schema bundle

- A small step in `crates/proto` (a bin or a `just` target) that compiles the 19 files with `protox`, writes `omp.binpb` (a descriptor set) and a tarball of the `.proto` sources plus `buf.yaml`, and records `SCHEMA_REV` and the git revision in a manifest.
- Acceptance: the bundle contains the 19 files; the descriptor set decodes and lists the six services and 63 RPCs; the manifest's revision equals `SCHEMA_REV` (`crates/proto/src/lib.rs:51`); two builds are byte-identical.
- Gate: a `just` target, not part of the default build.
- Tests: a Rust test that decodes the descriptor set and asserts the service and RPC counts, so adding a service forces a deliberate update.

### Slice 2: ADR 0048 amendment

- Record the owner's ruling on client generation: which tool (`buf generate` with local plugins, or `protoc`), that output is never committed, and where artifacts are published. Fix the stale "does not run `buf breaking`" sentence.
- Acceptance: the amendment names the generator, its pinned version and the licence check. CI already pins buf 1.73.0 (`.github/workflows/ci.yml:170-208`).

### Slice 3: Python client (`omp-grpc`)

- Generate modules from the bundle in a release job with `grpcio-tools`. Hand-write a thin layer: a UDS channel helper, TLS and bearer-token credentials, the `Hello` handshake that refuses a server whose `min_schema_rev` exceeds the client's revision, and receive limits raised to the server's 64 MiB. Cover `Gateway`, `Inference`, `Auth` and `Blob`; leave out `SessionControl` until a server exists.
- Acceptance: against a live daemon on a socket, `Hello` returns the three capabilities, one unary `Auth` call succeeds, one `Inference` server stream delivers chunks, a `Blob` upload (client stream) round-trips; a client older than `min_schema_rev` fails with a typed error.
- Tests: a conformance script run against `omp` in CI as an opt-in job; no network.
- Name: `omp-grpc`. No `pi` identity anywhere.

### Slice 4: Go client

- Prerequisite: Go packages cannot import each other in a cycle, and `thread` and `inference` do. Map both proto packages to one Go import path in a generation-only configuration (buf managed mode or `M` flags), without editing the `.proto` files. Whether protoc-gen-go accepts two proto packages in one Go package is UNVERIFIED; if not, the cycle has to be broken in the schema, which would be a breaking proto change and needs its own decision.
- Acceptance: `go build` and `go vet` pass on the generated module; the same conformance script runs in Go.
- Gate: opt-in CI job; Go is installed only there.

### Slice 5: conformance and versioning

- One language-neutral scenario list (hello, unary, server stream, client stream, large message, schema mismatch) run by every client against one daemon.
- Acceptance: every client passes the same list. Client packages carry the `SCHEMA_REV` they were generated at, and a release fails if the bundle and a client disagree.

## 6. Open questions

1. **ADR 0048.** Does the owner accept `buf generate` or `protoc` for non-Rust clients as a release step? The ADR's rejection was written about the Rust build. Without an answer, slices 3 and 4 do not start.
2. **Go in the repository.** Is a Go toolchain in CI acceptable under "no new language runtimes", or must Go generation live in a separate repository?
3. **The thread/inference cycle** (above). UNVERIFIED whether one Go package for both proto packages generates cleanly.
4. **Scope.** Should a client cover the five daemon services only, or also `SessionControl`, which a different server (the session host) exposes on a different socket?
5. **The stdio `omp rpc` protocol** stays hand-written and untyped in its payloads. A typed client for it needs a schema first. Is that wanted, or is gRPC the supported programmatic surface?
6. **Compatibility promise.** `buf breaking` uses WIRE and ignores four packages. A published client implies a promise for the other eleven. The owner should say which services are stable.
7. **Large messages.** Clients must raise receive limits to 64 MiB; a server-side lower cap for remote TCP may be wise. Not evaluated.
8. No conflict with ADR 0036, 0024 or 0006. ADR 0048 is the only locked decision in the way.

## 7. Upstream origin and license

Option 7 would reuse upstream's SDKs for the stdio protocol if the compatibility probe passes. Otherwise nothing from them is reused. Useful ideas either way, the useful ones are the binding rules (strict enums, an `Extra` map for open records, strict closed objects) and the split between a generated wire file and a hand-written client (`upstream:sdk/go/omp-rpc/README.md:22-29`). Upstream's `LICENSE` is MIT, with copyright lines for Mario Zechner, Can Bölük and Stencil Labs, Inc. (`upstream:LICENSE:1-5`); the Python SDK carries its own MIT file (`upstream:sdk/python/omp-rpc/LICENSE:1-3`).

Generator tools (buf, protoc-gen-go, grpc-go, grpcio-tools) are separate projects with their own licences. None was checked in this checkout (UNVERIFIED), and `deny.toml` covers only Rust crates, so a licence review would be a manual step.

