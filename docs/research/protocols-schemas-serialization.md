# OMP² Protocols, Schemas, Serialization and Durable Formats

Status: research complete; no implementation started. The CI and backlog items below are proposals for owner review (ADR 0048).
Date: 2026-10-11

This document is based on four read-only evidence passes over `origin/omp2` (worktree `/private/tmp/omp-proto-ci` @ `0f3fc1342d`): the schema inventory, the serialization survey, the Buf/Bazel/REAPI review and the durable formats audit. Nothing was built, tested, benchmarked or run through `buf` during any of them.

**Tags**

- **OBSERVED**: read in the repository at the cited `path:line` or `path:symbol`.
- **V**: an external claim checked this session against upstream docs, source, a registry index or RustSec.
- **T**: an external claim from training knowledge that was not checked.
- **[INFERENCE]**: a conclusion drawn from the evidence, not something observed directly.
- **UNVERIFIED**: a number or behavior nobody has measured.

## Summary

- OMP² already splits the layers sensibly:
  - Protobuf (protox + prost/tonic, compiled in `build.rs`) for process and network boundaries.
  - JSON inside SSE frames for the durable journal.
  - postcard for private local stores.
  - JSON-RPC wherever an external standard requires it (ACP, MCP, LSP/DAP).

  No serialization migration is justified.
- **A schema registry is not needed.** Schemas versioned in git are enough. The one real gap is that wire compatibility is not checked automatically.
- **`buf breaking` is justified now.** It should use category `WIRE`, run against the PR base, cover the packages whose two ends can run different builds, and ignore `omp/control`, `omp/telemetry`, `omp/ui` and `omp/collab`.
- Buf stays limited to format, lint and breaking checks. `buf generate`, the BSR, Bazel proto rules and BuildBuddy caching for proto generation are all rejected. Each duplicates `build.rs` and solves no measured problem.
- Four concrete decode risks in durable data are implementation backlog items, not CI changes:
  - the strict `tool.result@1` on replay;
  - the closed kind vocabulary failing the whole journal on open;
  - uncapped journal reads;
  - protobuf preflight bounds that production never calls.

## 1. Protocol inventory

### 1.1 Owned schemas: 18 files, 14 packages

OBSERVED: `crates/proto/proto/omp/**`. All 18 files are `syntax = "proto3"`, every package ends in `v1`, and there are 9,258 lines in total. Counts are messages / enums / oneofs / RPCs.

| File | Package | Counts | Purpose | Rust consumers (`omp_proto::<pkg>`) |
|---|---|---|---|---|
| `auth/v1/auth.proto` | `omp.auth.v1` | 47 / 6 / 2 / 21 (`service Auth`) | Credential broker | app, serve; `AuthClient` in driver (`auth_backend.rs:703`) |
| `blob/v1/blob.proto` | `omp.blob.v1` | 7 / 0 / 0 / 4 (`service Blob`) | SHA-256 CAS blob transfer | app, e2e, env, envd, serve |
| `collab/v1/collab.proto` | `omp.collab.v1` | 40 / 8 / 3 / 0 | Collaboration relay frames, in-memory only (the wire is hand-mapped JSON) | collab, app, driver, e2e |
| `control/v1/control.proto` | `omp.control.v1` | 35 / 6 / 3 / 0 | Agent spawn, budgets, ledger | No direct consumer; reached only through toolhost Control envelopes (`toolhost.proto:1347-1355`) |
| `document/v1/document.proto` | `omp.document.v1` | 106 / 25 / 12 / 0 | Document authority, leases, LSP/DAP | envd (24 sites), env, app, driver, e2e |
| `env/v1/env.proto` | `omp.env.v1` | 209 / 31 / 26 / 0 | envd `ClientFrame`/`ServerFrame` | envd (59), env (13), app, driver, agent, tool, tools, e2e, py |
| `gateway/v1/gateway.proto` | `omp.gateway.v1` | 3 / 0 / 0 / 2 (`Gateway`, `ForwardProxy`) | gRPC preflight `Hello`, forward proxy | rpc, app |
| `identity/v1/identity.proto` | `omp.identity.v1` | 8 / 0 / 0 / 0 | Opaque ids, effect envelopes | Through `env.proto:1359-1361` only |
| `inference/v1/{common,inference,media,models,search}.proto` | `omp.inference.v1` | 134 / 45 / 15 / 30 (`service Inference`) | Turns, realtime, media, search, catalog, `ValueMap` | ai, app, chat, driver, e2e, envd, observability, serve, session, tool, tools |
| `policy/v1/policy.proto` | `omp.policy.v1` | 15 / 1 / 0 / 0 | Shell-effect IR, denials | envd (20), driver, app, env, shell, tool |
| `telemetry/v1/telemetry.proto` | `omp.telemetry.v1` | 21 / 0 / 1 / 0 | Telemetry events | No direct consumer; toolhost envelopes and `tests/bytes_text.rs` only |
| `thread/v1/thread.proto` | `omp.thread.v1` | 11 / 4 / 2 / 0 | Canonical conversation AST | agent (16), driver (20), session (14), app, ai, envd, e2e |
| `toolhost/v1/toolhost.proto` | `omp.toolhost.v1` | 163 / 14 / 24 / 0 | Python host/worker protocol | agent (22), envd, app, driver, e2e |
| `ui/v1/ui.proto` | `omp.ui.v1` | 56 / 1 / 8 / 0 | TML and UI effects | envd (4) |

Totals: 855 messages, 141 enums, 109 oneofs, 5 services and 57 RPCs.

- **Consumers.** The 18 crates depending on `omp-proto` are: agent, ai, app, chat, collab, driver, e2e, env, envd, observability, rpc, serve, session, shell, tool, tools, plus others found by grep of `crates/*/Cargo.toml`.
- **Well-known types.** `google/protobuf/struct.proto` comes from protox's embedded copy (`crates/proto/build.rs:44-47`). It is not checked in.
- **Import cycle.** `thread.proto` imports `inference/v1/common.proto`, and `inference.proto` imports `thread/v1/thread.proto`. That is why `buf.yaml` excepts `PACKAGE_NO_IMPORT_CYCLE` (`crates/proto/buf.yaml`).

### 1.2 Vendored third-party schemas

There are 21 files under `fixtures/llm-oracle/vendor-schemas`, with 2,324 messages, 229 enums, 15 services, 435 RPCs and 20,202 lines:

- `cursor/agent.proto` (package `agent.v1`);
- 19 Devin `exa.*` files;
- `buf/validate/validate.proto`.

Controls (OBSERVED):

- `NOTICE.txt` excludes these files from the MIT license, because they come with no license grant.
- `crates/ai/build.rs` compiles them with plain `prost-build` and generates no gRPC stubs.
- The Cursor file is pinned by `CURSOR_SCHEMA_SHA256`.
- The Devin files are checked against `manifest.json`, the commit `fc01e3b6…`, a closure of 20 files, and an empty `drift.json`.
- `buf.yaml` does not cover them: its module path is `proto` relative to `crates/proto`.

Measured by message count, the vendor pipeline is larger than the owned one.

### 1.3 REAPI and googleapis

None are vendored yet. The only `.proto` files in the repository are the 18 owned files and the 21 vendor files (OBSERVED, glob). The `google.rpc.*` strings in `crates/ai` are JSON type URLs inside provider error bodies, not schemas. ADR 0040 PR 1b plans the vendoring (§10).

### 1.4 Code generation (OBSERVED, `crates/proto/build.rs:main`)

1. `root` is read from `CARGO_MANIFEST_DIR` at run time, not with `env!`, so worktrees can share a target directory.
2. `cargo::rerun-if-changed` is emitted for the directory and for each `.proto` file.
3. `protox::compile` builds one `FileDescriptorSet`.
4. `bytes_field_attributes` adds `#[serde(with = bytes_text…)]` to every `bytes` field.
5. `tonic_prost_build::configure()` is called with:
   - `.bytes(".")`, `.btree_map(".")`, `compile_well_known_types(true)`;
   - serde derives and `#[serde(default)]`;
   - client/server stubs gated on `CARGO_FEATURE_TONIC`.
6. `compile_fds` writes `omp.<pkg>.v1.rs` into `OUT_DIR`, where `src/lib.rs:71-398` `include!`s it.

Other facts:

- Generated code is never committed.
- There is no system `protoc` (`AGENTS.md:616`).
- Pinned versions: protox 0.9.1, prost 0.14.4, tonic and tonic-prost-build 0.14.6 (`Cargo.toml:189-192,239-242`).
- The workspace always enables `omp-proto/tonic` (`Cargo.toml:148`).
- The comment at `build.rs:38-41` says a directory `rerun-if-changed` "misses edits to nested `.proto` files". The Cargo Book says the directory is scanned recursively (V). The per-file lines are therefore redundant but harmless; only the comment is wrong.

## 2. Per-boundary table

"Different builds?" asks whether a production deployment can have the two ends of the boundary built from different commits. That column drives §11.

| Boundary | Encoding | Schema | RPC semantics | Transport | Different builds? | Evidence |
|---|---|---|---|---|---|---|
| app/peer ↔ project `envd` | Protobuf, varint-delimited | `omp.env.v1` (+ identity, policy, inference `ValueMap`, blob) | Custom `request_id` multiplexing; id 0 for hello/events; cancel by target | Owner-only UDS, Windows named pipe, stdio, in-process | **Partly.** Socket keyed by executable generation, so a rebuilt `omp` binds its own listener. A newer client still decodes a stale owner's `ServerHello` and sends `Retire`. Hello accepts `MIN_SCHEMA_REV=4..=SCHEMA_REV`. Every other op requires `wire_revision == SCHEMA_REV` | `env/project_state.rs:117-124`; `envd/server.rs:186,4052`; `envd/lib.rs:1974-2004,2065-2086`; `env/build_id.rs:24-36`; `envd/exec.rs:839,997,1008,1028`; `workspace/operations.rs:845` |
| `envd` ↔ document authority (docserver) | Protobuf, length-delimited | `omp.document.v1` | `PROTOCOL_MAJOR=2`/`MINOR=2` negotiation + `server_build` | Owner UDS or Tokio duplex | **Yes.** The document socket is deliberately build-stable; a current-build envd can attach to an older docserver | `docserver/connection.rs:46-48,470-497`; `env/project_state.rs:124-125`; `envd/README.md:207-212` |
| ext host/worker ↔ `envd` DATA | Protobuf | `omp.env.v1` + `InvocationScope` | env protocol | Per-host UDS | No: the child re-execs `current_exe()` | `envd/worker.rs:529`; `exthost/spawn.rs:46`; `py/src/bindings.rs:4089` |
| `envd` ↔ ext host CONTROL | 4-byte BE length + JSON, with protobuf toolhost envelopes as bytes inside | `omp.toolhost.v1` (+ control, telemetry, ui envelopes) | Custom | Inherited fd 3 socketpair | No: same binary | `exthost/control.rs:3755,3788-3803`; `dispatch.rs:786-799,1166-1209` |
| `envd` ↔ managed process `ReadyPing` | Protobuf | `toolhost.v1` `HostFrame{Ping}` → `WorkerFrame{Pong}` | Probe | Process stdio | **Yes, potentially** [INFERENCE]: the probed process is user- or extension-supplied | `envd/exec.rs:3811-3830,3919-3951`; `py/python/omp/env.py:1649-1650` |
| `envd` ↔ placed Python worker `WorkerHello` | Protobuf | `toolhost.v1` `WorkerHello{schema_rev,…}` | Hello | Piped stdio (per docs) | Same binary by design; **no Rust validator found** | grep; `docs/architecture/processes.md` names a `WorkerProcess::spawn` that exists only in `crates/memory` (doc drift) |
| Client ↔ `omp serve` / auth-broker / auth-gateway | Protobuf | `omp.gateway.v1`, `auth.v1`, `blob.v1`, `inference.v1` (+ `thread.v1`) | gRPC unary + streaming; `Hello` preflight | tonic over owner UDS, or TCP + bearer token | **Yes.** Long-lived independent daemons; TCP may be remote. **But** `chat --gateway` only calls `endpoint.connect()`, and `omp_rpc::handshake` is called only from its own test | `app/daemon.rs:337-370`; `rpc/src/hello.rs:12,101-118`; `gateway.proto:9-13`; `app/chat_cmd.rs:541-543`; `auth_gateway_cmd.rs:85-96` |
| Collab host ↔ relay ↔ guest/browser | Hand-written JSON in an AES-GCM envelope | `collab.v1` structs in memory only | `PROTOCOL_REVISION=3`, exact equality | WebSocket (`wss://my.omp.sh`) | Yes (other machines, collab-web) | `collab/codec.rs:1-2,55,92-96`; `collab/lib.rs:15`; `relay.rs:85-101` |
| Editor ↔ `omp` ACP | JSON-RPC 2.0 | ACP | `protocolVersion == 1` | stdio | Yes (third party) | `app/acp_mode.rs:122-125,463-465` |
| SDK ↔ `omp rpc` | NDJSON | `rpc` protocol | `PROTOCOL_V1`/`V2` negotiation | stdio | Yes | `rpc/protocol.rs:17-19`; `app/rpc_mode.rs:3236`; `rpc/src/framing.rs:17-19` |
| `omp` ↔ Tern (TSP) | JSON / base64 in APC | TSP | `hello{v:[1]}` | pty in-band | Yes | `tui/src/tsp/mod.rs:26-61`; ADR 0041 |
| `envd` ↔ MCP servers | JSON-RPC | MCP dated revisions (`2025-11-25` + 3 older) | MCP | stdio / streamable HTTP / legacy SSE | Yes | `envd/mcp/config.rs:21-23` |
| docserver ↔ LSP/DAP | `Content-Length` JSON-RPC | LSP/DAP | Standard | stdio | Yes | `docserver/lsp_process.rs:50-58,1041`; `dap_protocol.rs:31-32` |
| `envd` ↔ eval child; memory embedding worker | NDJSON (`deny_unknown_fields`) | Private | Request/response | stdio | No [INFERENCE] | `memory/embedding/protocol.rs:29,116` |
| Journal `.oms` | SSE frames + single-line JSON | `Kind{name,rev}` (`tool.result@1`, …) | Append/replay | File | **Yes**: new builds replay old sessions; downgrades open newer ones | `journal/src/sse.rs:1-15`; `kind.rs:102-117` |
| Blob CAS | Raw bytes | SHA-256 address | — | `<root>/blobs/aa/bb/<sha256>` | Yes | `journal/src/blob.rs:1-19,662` |
| `envd` `processes/meta.json` | JSON containing protobuf bytes as number arrays | `PROCESS_STORE_VERSION=1` + `env.v1` `ProcessSpec`/`ReadyProbe`/`ExecStatusMsg` | Recovery | File | **Yes** [INFERENCE]: a successor durable owner, possibly a newer build, recovers the records | `envd/process_store.rs:18,84-92,202-231`; `exec.rs:1469-1478,1543-1546,1732` |
| AI conversation store | postcard BLOBs in SQLite; one row embeds an `omp_native` revision protobuf | None | — | SQLite | Yes | `ai/src/session/store.rs:329-345,485,509,842`; `omp_native.rs:410` |

Smaller drift found along the way (OBSERVED):

- `crates/py/python/omp/limits.py:39` sets `SCHEMA_REV = 7` while documenting it as shared with `omp_proto::SCHEMA_REV`, which is 27.
- `collab.v1.JournalRecord.transcript_v4_json` now carries `patch@1`/`snapshot@1`/`stream@1` JSON (`driver/src/collab/session.rs:2109-2137`).

## 3. Serialization technology comparison

Categories: RE = record/RPC encoding, SD = self-describing binary, ZC = zero-copy, COL = columnar, RS = Rust-native serde format. Rows marked ★ are in production in OMP².

| Tech | Cat | Schema | Evolution | Zero-copy | Rust crate status | Security note | OMP² verdict | Tag |
|---|---|---|---|---|---|---|---|---|
| ★ Protobuf | RE | `.proto` | Tag-based; `reserved`; prost **drops unknown fields**, keeps unknown enum values | No (`Bytes` fields slice in O(1)) | prost/tonic 0.14, active; **no Editions support** (prost #1031) | Recursion limit 100 by default (T); OMP adds `bounds.rs` preflight | Preserve | V |
| ★ JSON (serde_json 1.0.151) | SD (text) | Optional | Additive via `#[serde(default)]` | `RawValue` borrowing | Active | Default depth limit 128; `unbounded_depth` not enabled (`Cargo.toml:216`) | Preserve | V |
| ★ postcard 1.1.3 | RS | Shared Rust types | None beyond append discipline; wire stable since 1.0 | Borrowed `&str`/`&[u8]` | Active | Not self-describing | Preserve; add a version header to P1 (§9) | V |
| Avro | RE | JSON schema needed at read time | Writer/reader resolution | No | `apache-avro` active | Schema must be trusted | Reject: would need a second schema authority | V (crate) |
| FlatBuffers | ZC | `.fbs` | Append-only | Yes | 25.12.19 | RUSTSEC-2021-0122 (CVSS 9.8); always run `Verifier` | Reject without measurement | V |
| FlexBuffers | ZC+SD | No | Schemaless | Yes | No public verifier; pre-1.0 API | `get_root` is not validation | Reject | V |
| Cap'n Proto | ZC+RPC | `.capnp` | Ordinal append | Yes | capnp 0.22 active | Reader traversal limits | Reject: no integration path | V |
| Thrift | RE+RPC | `.thrift` | Field ids | No | 0.25.0; Rust maintenance gap early 2026, since restored | 0.25 adds a depth guard | Reject | V |
| MessagePack | SD | No | Ad hoc | No | rmp-serde active | Bound size and depth yourself | Reject: no need | V/T |
| CBOR | SD | CDDL optional | Ad hoc | No | ciborium, minicbor 2.x; `serde_cbor` unmaintained (RUSTSEC-2021-0127) | — | Reject: no need | V |
| BSON | SD | No | Ad hoc | Partial | bson 3.1 | Larger than CBOR | Reject | V |
| Arrow / Parquet / ORC | COL | Yes | Column add | Columnar | arrow-rs active; orc-rust less mature | Validate offsets; decompression bombs | Defer: offline analytics export only | V/T |
| Bond | RE | Yes | Tags | No | **EOL 2025-03-31** | Unsupported | Reject | V |
| SBE | ZC | XML | `sinceVersion` | Yes | `sbe-tool` Rust target | — | Reject | V/T |
| bincode | RS | Rust types | None | No | **Unmaintained** (RUSTSEC-2025-0141); transitive only via `syntect` | — | Reject (first-party) | V |
| rkyv 0.8 | ZC (RS) | Rust types | **No schema evolution** | Yes | Active; transitive only via `dtls` | Use the validating `access` only | Reject for durable data | V |
| ASN.1 (rasn 0.29) | RE | Yes | Extension markers | No | Active | History of parser CVEs | Reject | V/T |

Current first-party use, OBSERVED from `Cargo.lock` and call-site counts:

- **prost:** proto, ai, collab, driver, e2e, envd.
- **serde_json:** about 32 crates.
- **postcard:** ai, catalog, and core (dev-only).
- **rusqlite:** ai, cache, chat, driver, envd, memory, tools, app.
- **Other formats:** toml/serde_yaml for config, zerocopy (ar), bytemuck (gui), quick-xml (tools).
- **Absent from the lock file:** ciborium, rmp, flatbuffers, capnp, apache-avro, arrow, borsh, bitcode, musli and minicbor.

## 4. IDL comparison

★ marks the current choice. All cell judgments are [INFERENCE] from the facts in §3.

| IDL | Internal IPC | Remote RPC | Plugin iface | Tool APIs | HTTP APIs | Event contracts | Durable journal | Remote exec | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| Protobuf | ★ fit | ★ fit (tonic) | ★ fit (toolhost) | partial (model tool args are JSON Schema) | partial | fit | partial (OMP uses JSON) | ★ fit (REAPI is protobuf) | Preserve |
| JSON Schema | — | — | partial | ★ fit (LLM tool parameters, `schemars`) | partial | fit | partial (could validate `data`) | — | Preserve for tool schemas |
| Thrift / Cap'n Proto / FlatBuffers / Avro | fit or partial | fit or partial | partial | poor | poor | partial | partial | partial | Reject: duplicate IDL |
| Smithy | poor | fit | poor | partial | fit | partial | poor | partial | Reject: smithy-rs server APIs unstable, Gradle-based (V) |
| OpenAPI / TypeSpec | poor | partial | poor | partial | fit | poor | poor | partial | Defer until a public HTTP API exists; TypeSpec Rust emitter is client-only (V) |
| AsyncAPI | poor | poor | poor | poor | poor | fit | partial | poor | Defer: no broker |
| GraphQL SDL | poor | partial | poor | poor | fit | partial | poor | poor | Reject |
| WIT (WASI 0.3, stable 2026-06-11, V) | in-process only | poor | fit | fit (sandboxed tools) | poor | poor | poor | partial | Defer: relevant only if a wasm plugin host is approved |
| ASN.1 / CUE | partial / poor | — | — | — | — | — | — | — | Reject |

## 5. Separating RPC from transport

OMP² already keeps serialization, schema, RPC semantics and transport as separate layers (OBSERVED):

| Mechanism | Serialization | Schema | RPC semantics | Transport | Used by OMP² |
|---|---|---|---|---|---|
| OMP env protocol | Protobuf, varint-delimited | `omp.env.v1` | `request_id` multiplexing, cancel by target, `SCHEMA_REV` Hello, `build_id` staleness | stdio / UDS / named pipe / in-process | ★ |
| gRPC (tonic) | Protobuf | `.proto` | unary + 3 streaming kinds, deadlines, status | HTTP/2 | ★ `omp-serve` |
| JSON-RPC 2.0 | JSON | defined per protocol | request/response/notification | any byte stream | ★ ACP, MCP, LSP |
| NDJSON RPC | JSON lines | `rpc` protocol | v1/v2 negotiation | stdio | ★ `omp rpc` |
| WebSocket | hand-mapped JSON + AES-GCM | collab rev 3 | exact revision | TCP + upgrade | ★ collab |
| Connect RPC | Protobuf or JSON | `.proto` | gRPC-compatible | HTTP/1.1, 2 | not used; `connectrpc` Rust is 0.9, pre-1.0 (V) |
| gRPC-Web, QUIC, HTTP/3, NATS, MQ | — | — | — | — | not used |
| WASM Component Model | Canonical ABI | WIT | typed calls | in-process | not used |

The consequence: swapping gRPC for Connect, or UDS for QUIC, would change one column and leave the schema alone. No transport change is proposed. ADR 0044 already requires a concrete process or machine boundary before any new transport is introduced.

## 6. Schema registry verdict

**Git-versioned schemas are enough. Reject every registry for now** (BSR, Confluent, Apicurio, Karapace, AWS Glue, Azure Event Hubs, Redpanda).

Reasons:

1. **Every producer and consumer ships from this repository.** All `omp.*.v1` code is compiled in through `build.rs`. Nothing resolves schemas at runtime, which is the main job of Confluent, Glue and Azure registries (OBSERVED, §1.4).
2. **Compatibility is already negotiated in-band:**
   - `SCHEMA_REV`/`MIN_SCHEMA_REV` Hello (`proto/src/lib.rs:51`; `envd/server.rs:186`; `rpc/src/hello.rs:12`);
   - document `PROTOCOL_MAJOR/MINOR`;
   - envd `build_id` staleness.
3. **There is nothing for registry framing to attach to.** No Kafka or MQ stream carries schema ids.
4. **The durable journal is JSON with `Kind@rev`.** A protobuf registry cannot check it; golden fixtures can (§9).
5. **The only real gap is automated wire-compatibility checking.** `buf breaking` against a git baseline closes it offline, at no operating cost (§11).

Registry comparison (V unless noted):

| Registry | Formats | Compat modes | Offline | License | Fit |
|---|---|---|---|---|---|
| Buf CLI (no BSR) | Protobuf | FILE ⊃ PACKAGE ⊃ WIRE_JSON ⊃ WIRE against git/dir/image | Fully offline | Apache-2.0 (T) | **Adopt for `breaking` only** |
| BSR | Protobuf | Same + registry checks | Needs network | SaaS/on-prem; free tier allows 1 private repo of ≤100 types | Reject |
| Confluent SR | Avro/Proto/JSON Schema | BACKWARD/FORWARD/FULL (+TRANSITIVE) | Container | Confluent Community License (not OSI) | Reject |
| Apicurio 3.x | Many; AsyncAPI compat is validity-only | NONE…FULL_TRANSITIVE; GitOps mode | Container | Apache-2.0 | Revisit if a broker appears |
| Karapace | Avro/Proto/JSON Schema | Confluent-compatible | Container | Apache-2.0 | Revisit if a broker appears |
| AWS Glue SR / Azure EH SR | Avro/JSON/Proto | Glue `_ALL` modes; Azure enforces Avro only | No | Proprietary | Reject |
| Redpanda SR | Avro/Proto/JSON | BACKWARD default + TRANSITIVE | Built into broker | BSL core (T) | Reject |

Revisit when:

- independently released external SDKs consume these `.proto` files (then consider the BSR); or
- a broker-based event bus appears (then Apicurio or Karapace).

## 7. Buf assessment

- **The cache holds dependencies only (V).**
  - It lives at `$BUF_CACHE_DIR`, else `$XDG_CACHE_HOME/buf`, else `~/.cache/buf`.
  - It holds only BSR modules listed under `deps`, verified against `buf.lock` `b5:` digests.
  - Modules in the workspace are read from disk.
  - It caches no plugin outputs or generated code.
  - OMP² has no deps, no `buf.lock`, no `buf.gen.yaml` and no module `name` (OBSERVED), so the cache does nothing for us.
- **`buf generate` is not incremental (V).**
  - It regenerates every selected plugin × (directory or all) on every run.
  - Scope can only be narrowed by hand (`--path`, `--exclude-path`, `types`).
  - `clean: true` deletes the outputs first.
  - It would not beat Cargo's fingerprinting.
  - It would also duplicate `build.rs` and lose its customizations: serde derives, `BTreeMap`, `Bytes`, local well-known types, per-field bytes-text attributes (`build.rs:52-66`).
- **Remote plugins are not REAPI (V).** BSR remote plugins are a proprietary hosted codegen service that needs the network. They have no CAS, no ActionCache and no `Execute`.
- **Lint and format.** Already in CI: `.github/workflows/ci.yml:112-138` job `protobuf`, pinned buf 1.73.0 checked by SHA-256, running `buf format -d --exit-code` and `buf lint` with `BASIC`. `just proto-fmt-check` and `just proto-lint` match it locally (`justfile:46-59,149`). Keep the pinned-binary install rather than `bufbuild/buf-action@v1`: there is no push, no token and no third-party action.
- **Breaking.** This is the remaining value; see §11. V for the usage:
  - baselines can be `.git#branch=…,subdir=…`, `.git#ref=…`, a remote git URL, a directory or an image;
  - categories are FILE ⊃ PACKAGE ⊃ WIRE_JSON ⊃ WIRE;
  - `ignore`/`ignore_only` paths in `buf.yaml` v2 are relative to the directory that holds `buf.yaml`.

## 8. Bazel incremental generation vs cargo + build.rs + protox

**Verdict: keep cargo + `build.rs` + protox. Reject Bazel proto generation.**

| Strategy | Granularity | Remote cache | Conflict with OMP² |
|---|---|---|---|
| A. `proto_library` + `rules_rust_prost` (BCR 0.74.0, active, V) | One action per `proto_library` + rustc per crate | Native REAPI | Requires `protoc` (forbidden by `AGENTS.md:616`); loses `build.rs` customizations; needs the whole workspace in Bazel, which ADR 0040 rejects (`docs/adr/0040-…md:362-369,423`) |
| B. Buf as a Bazel action (`rules_buf` 0.5.4, "beta", V) | One coarse action | Only with local plugins | Everything in A, plus a second compiler; `rules_buf` does not generate code |
| C. `buf generate`, checked in | One run | None | Duplicate code + drift checks |
| **D. Hybrid: Buf for checks, Cargo for codegen** | — | — | **What OMP² does today** |

`bazelbuild/rules_proto` is deprecated (V).

The current pipeline (OBSERVED):

- The unit is the crate.
- An unrelated `.rs` edit does not rerun the build script.
- A proto edit regenerates all 14 packages and recompiles `omp-proto` plus its 18 dependents.
- Bazel A could only do better if `omp-proto` were split per package. Even then the rustc recompile of the dependents dominates under any generator [INFERENCE].
- The protox + prost step itself is guessed at tens to hundreds of ms. **UNVERIFIED.**
- The one real lever is splitting `omp-proto` per package, and only if measurement shows it pays (§12, decision rule).

## 9. BuildBuddy remote caching and proto generation

This is not relevant to proto generation.

- In a Cargo build, codegen is not a separate action: it runs inside the `omp-proto` build script.
- sccache cannot cache build-script runs (T, consistent with ADR 0040 `:395-397`).
- BuildBuddy's Bazel remote cache needs Bazel, which is rejected.
- ADR 0040's coarse whole-command REAPI or BuildBuddy Run API actions (`just test-pkg <crate>`) cache codegen only as part of a whole command. That is how it should be.
- The pure-Rust pipeline already suits those actions: "No lld or protoc is needed on Linux" (ADR 0040 `:792`).

CI cost signal (OBSERVED):

- A PR runs both protox pipelines (`omp-proto` and `omp-ai` vendor) in at least four jobs: `rust` (macOS), `lint_linux`, `p9_p10_linux` and `p7_linux`.
- Each job has its own cache key.
- `remote_heavy` adds at least two more runs on push.
- This duplication comes from having separate runners; it is not specific to protobuf.
- Wall-clock cost is **UNVERIFIED**.

## 10. REAPI contract inventory, providers and Rust bindings

The REAPI contract was checked against `bazelbuild/remote-apis@main` `build/bazel/remote/execution/v2/remote_execution.proto` (V). The package is `build.bazel.remote.execution.v2`.

**Imports:**

- `build/bazel/semver/semver.proto`
- `google/api/annotations.proto`
- `google/longrunning/operations.proto`
- `google/protobuf/{any,duration,timestamp,wrappers}.proto`
- `google/rpc/status.proto`

**Services:**

| Service | RPCs | Notes |
|---|---|---|
| `Execution` | `Execute` (stream `Operation`), `WaitExecution` | — |
| `ActionCache` | `GetActionResult`, `UpdateActionResult` | — |
| `ContentAddressableStorage` | `FindMissingBlobs`, `BatchUpdateBlobs`, `BatchReadBlobs`, `GetTree`; `SplitBlob`/`SpliceBlob` (v2.12, removed v2.13); `GetChunkMapping`/`RegisterChunkMapping` (v2.13, gated on `split_blob_support`/`splice_blob_support`) | Chunking is not needed |
| `Capabilities` | `GetCapabilities` | `deprecated_api_version`/`low_api_version`/`high_api_version` (SemVer) |
| `google.bytestream.ByteStream` | `Read`, `Write`, `QueryWriteStatus` | `{instance}/uploads/{uuid}/blobs/{digest_function/}{hash}/{size}`, `compressed-blobs/{compressor}/…` |
| `google.longrunning.Operations` | `List/Get/Delete/Cancel/WaitOperation` | Unsupported methods → `UNIMPLEMENTED` |

**Key messages:**

- `Action`, `Command` (`output_paths = 7`; `output_files`/`output_directories` deprecated since v2.1), `Platform`, `Directory`, `Digest`;
- `ActionResult`, `Tree`, `ExecuteRequest`/`ExecuteResponse`, `ExecuteOperationMetadata`;
- `ServerCapabilities`, `CacheCapabilities`, `ExecutionCapabilities`, `RequestMetadata`;
- `DigestFunction` (`SHA256 = 1`, `BLAKE3 = 9`).

Negotiation works by discovery through `GetCapabilities`, not by handshake.

ADR 0040's REAPI names all match upstream. Its line numbers come from pinned commit `6def1c5d…` and drift by ±1 from `main`. Its "Split/Splice" row predates the v2.13 rename, which does not matter for slice 1.

**Provider matrix** (API ranges from the upstream remote-apis README, V unless noted):

| Server | REAPI | Remote exec | `CancelOperation` | Auth |
|---|---|---|---|---|
| BuildBuddy | 2.0–2.11 | yes | Not found; per-invocation `BuildBuddyService.CancelExecutions` (ADR 0040 `:1091`) | `x-buildbuddy-api-key` header or mTLS |
| NativeLink | 2.0–2.3 | yes | **`unimplemented`** (V); `client_action_timeout_s` | mTLS + network isolation (V-moderate) |
| EngFlow | 2.0–2.11 | yes | Yes with `--enable_invocation_cancellation` (V) | mTLS or `ef` credential helper |
| Buildbarn | 2.3–2.11 | yes | Admin `KillOperations` [INFERENCE/T] | TLS client certs and/or JWT (V-moderate) |
| BuildGrid | 2.0–2.2 | yes | Implemented, ACL-gated (V-moderate) | ACL config (T) |
| Buildfarm | 2.3–2.11 | yes | T | T |
| bazel-remote | 2.0–2.3 | no (cache only) | n/a | — |

What this implies for the client [INFERENCE]:

- Target 2.0–2.3 semantics: `output_paths`, SHA256, ByteStream for large blobs, and batching under `max_batch_total_size_bytes`.
- Do not depend on chunking.
- Probe for cancellation: map `UNIMPLEMENTED` to `Unsupported`, then fall back to the vendor profile or `Action.timeout`.
- Support both header-key and client-certificate auth.

**Rust bindings:**

| Option | Status | Fit |
|---|---|---|
| Generate ourselves (ADR 0040 PR 1b) | — | **Recommended.** Same toolchain as `crates/proto/build.rs` |
| `bazel-remote-apis` 0.30.0 (2026-10-07, V) | Same prost 0.14 / tonic 0.14 as the workspace; checked-in generated sources; optional pbjson serde; single maintainer; pinned REAPI commit unknown | Fallback only |
| `reapi` 0.3.0 | prost 0.13 / tonic 0.12, which would add a second major | Reject |

**Recommendation: keep ADR 0040 PR 1b as written, and keep it deferred.**

- PR 1b (`docs/adr/0040-remote-build-and-test-execution.md:1194-1202`) vendors under `crates/exec-reapi/proto/`:
  - REAPI v2 `remote_execution.proto` and `semver.proto`;
  - googleapis `bytestream`, `longrunning/operations`, `rpc/status`, `rpc/error_details` and the `api` annotations;
  - behind the `buildbuddy-cancel` feature, the BuildBuddy cancel protos.
- It compiles them with protox + tonic-prost-build, exactly like `crates/proto/build.rs`.
- The vendored protos must be a separate Buf scope, or none at all. Keep them out of `crates/proto/buf.yaml`'s BASIC lint and the breaking check, because they are third-party.
- Pin the upstream commit and checksum them the way `crates/ai/build.rs` pins the vendor schemas.
- ADR 0040 is `proposed`, and slice 1 has not started. Its 2026-10-10 amendment makes the BuildBuddy Run API the optimized backend and REAPI the portable one (`:1467-1485`).

## 11. CI changes justified now

### 11.1 Inputs to the decision

Facts from the handshakes (OBSERVED):

- `omp_proto::SCHEMA_REV = 27` is one global counter for all 14 packages (`crates/proto/src/lib.rs:51`). Its doc comment says it is bumped for every wire-visible change. Nothing checks that.
- envd Hello accepts `MIN_SCHEMA_REV = 4 ..= SCHEMA_REV` (`envd/server.rs:186,4052`). Beyond Hello and `Retire`, every operation requires `wire_revision == SCHEMA_REV`, so non-Hello env traffic fails closed across revisions.
- Gateway Hello: the server accepts `MIN_SCHEMA_REV = 1`, and the client must abort if `server.schema_rev < client` (`rpc/src/hello.rs:12,101-118`; `gateway.proto:9-13`: "the gateway must never be older than its client … prost does not retain those unknown fields"). `chat --gateway` does not run Hello (`app/chat_cmd.rs:541-543`).
- The document protocol negotiates `PROTOCOL_MAJOR=2`/`MINOR=2` separately, against a deliberately build-stable docserver.
- `processes/meta.json` stores `env.v1` protobuf bytes that a successor envd decodes (`exec.rs:1543,1637,3305-3311`).
- The written evolution rules are in `env.proto:17-23` and `toolhost.proto:17-23`:
  - skip unknown fields and enums;
  - tag 15 `ValueMap props`;
  - never reuse field numbers;
  - reserve removed fields;
  - toolhost oneof arms stay fixed.

  Existing `reserved` statements show the rules are followed by hand (§1; e.g. `ClientFrame 41/"acp_exec_event"`). Nothing enforces them.

The "different builds?" column from §2, by package:

| Package | Ends can differ? | In breaking scope? |
|---|---|---|
| `omp.env.v1` | Partly: Hello/Retire, `meta.json` records | **Yes** |
| `omp.identity.v1`, `omp.policy.v1` | Embedded in `env.v1` (policy IR also travels base64 inside tool-call JSON, `envd/tools.rs:2241`) | **Yes** |
| `omp.document.v1` | Yes: build-stable docserver | **Yes** |
| `omp.gateway.v1` | Yes: independent daemons, TCP | **Yes** |
| `omp.auth.v1`, `omp.blob.v1` | Yes: gRPC services on those daemons; blob is also nested in env | **Yes** |
| `omp.inference.v1`, `omp.thread.v1` | Yes: gRPC `Inference`; `ValueMap` inside persisted env records; durable `omp_native` revision protobuf (`omp_native.rs:410`) | **Yes** |
| `omp.toolhost.v1` | Mostly no; **but** `HostFrame{Ping}`/`WorkerFrame{Pong}` is spoken by user- or extension-supplied processes for `ReadyPing` | **Yes** (cheap: its own rules already demand WIRE discipline) |
| `omp.control.v1`, `omp.telemetry.v1`, `omp.ui.v1` | No: same-binary toolhost envelopes only; ADR 0039 (proposed) plans deletions | **Ignore** |
| `omp.collab.v1` | Ends differ, but the wire is hand-written JSON, so protobuf tags never travel | **Ignore** |

### 11.2 Decision: `buf breaking` is justified now (Adopt)

The ends of `env`, `identity`, `policy`, `document`, `gateway`, `auth`, `blob`, `inference`, `thread` and the toolhost `Ping`/`Pong` subset can be built from different commits. The schemas carry written field-number rules, and prost silently drops unknown fields. A reused or retyped tag on those packages would therefore cause silent misdecoding across builds, not a clean failure. `buf breaking` is the only cheap check for this. It runs offline on the buf binary the `protobuf` job already installs.

**Category: `WIRE`.**

- `FILE` and `PACKAGE` protect generated-source compatibility. Generated Rust is never committed and every consumer compiles in this workspace, so `rustc` already catches source breaks.
- `WIRE_JSON` protects proto3 JSON names. OMP²'s serde JSON is Rust-native, not proto3 JSON (`build.rs:12-15`; `lib.rs:14-17`), and collab's JSON is hand-mapped. JSON-name checks would block renames with no benefit.
- `WIRE` includes `FIELD_NO_DELETE_UNLESS_NUMBER_RESERVED` (V), the repository's own written rule.
- T: `WIRE` does not block deleting a whole message, which leaves ADR 0039's planned deletions possible.

**Config:** add to `crates/proto/buf.yaml`. Paths are relative to the `buf.yaml` directory (V).

```yaml
breaking:
  use:
    - WIRE
  ignore:
    # Same-binary toolhost envelope payloads (ADR 0048 §11): both ends always
    # come from one build, and ADR 0039 plans owner-reviewed deletions here.
    - proto/omp/control
    - proto/omp/telemetry
    - proto/omp/ui
    # collab.v1 types are in-memory only; the relay wire is hand-mapped JSON
    # versioned by collab PROTOCOL_REVISION (crates/collab/src/codec.rs).
    - proto/omp/collab
```

There is no `except:` list. No rule in `WIRE` conflicts with a documented OMP² practice. Add an exception only when a real false positive shows up, with the reason written in the file.

**CI command.** Add these two steps to the existing `protobuf` job in `.github/workflows/ci.yml`, after `Lint Protobuf schemas`. They run on pull requests only; on push, the PR check has already covered the change.

```yaml
      - name: Fetch PR base for the breaking check
        if: github.event_name == 'pull_request'
        env:
          BASE_REF: ${{ github.base_ref }}
        run: git fetch --no-tags --depth=1 origin "+refs/heads/${BASE_REF}:refs/remotes/origin/${BASE_REF}"
      - name: Check Protobuf wire compatibility against the PR base
        if: github.event_name == 'pull_request'
        env:
          BASE_REF: ${{ github.base_ref }}
        run: |
          set -euo pipefail
          base="$RUNNER_TEMP/proto-base"
          mkdir -p "$base"
          git archive "refs/remotes/origin/${BASE_REF}" crates/proto | tar -x -C "$base"
          buf breaking crates/proto --against "$base/crates/proto"
```

Why this form:

- The checkout is the default depth 1 (`actions/checkout` with no `fetch-depth`, OBSERVED at `ci.yml:119`), so the base branch is not available locally.
- One `--depth=1` fetch of the base, plus `git archive` into a directory, gives buf a plain directory baseline. That avoids `.git#…` clone semantics on a shallow repository [INFERENCE] and needs no token or network beyond the existing `origin`.
- `buf breaking` uses the `breaking:` configuration of the input module [T]. So the first PR, which adds the section, is already checked under the new rules even though the base `buf.yaml` has none.

The equivalent local or full-clone invocation, from the repository root:

```sh
buf breaking crates/proto --against '.git#branch=omp2,subdir=crates/proto'
```

Add this as `just proto-breaking` and keep it out of `remote-heavy`, as with the other buf recipes (`justfile:137-139`). Before enabling the CI step, the main agent should run it once locally on the current tree to confirm the baseline passes.

**What this check does not cover, stated so it is not over-trusted:**

- It compares the PR against the base only, pairwise. It does not test the gateway's promise to accept `MIN_SCHEMA_REV = 1` clients at revision 27.
- It does not test the `env` Hello range 4..27.
- It does not check that `SCHEMA_REV` is bumped.
- It does not cover the journal, `meta.json`'s JSON shell, collab JSON or postcard.

### 11.3 Other CI candidates

| Candidate | Decision | Reason |
|---|---|---|
| `buf breaking` with `WIRE`, scoped as above | **Adopt** | §11.2 |
| `WIRE_JSON` or `FILE` category | **Reject** | Serde JSON is not proto3 JSON; no out-of-workspace generated code |
| `buf breaking` on `omp/collab` | **Reject** | Wire is hand-mapped JSON |
| `buf breaking` / lint on vendored `fixtures/llm-oracle/vendor-schemas` or future REAPI protos | **Reject** | Third-party; already pinned by SHA-256 or manifest |
| CI fails when `.proto` changes without a `SCHEMA_REV` bump | **Defer** | The rule is documented (`lib.rs:47-51`), but a textual diff check false-positives on comment edits, and exact `wire_revision` already makes env ops fail closed. Owner decision |
| Old-build ↔ new-build handshake tests (gateway rev-1 promise, docserver across builds) | **Defer** | Worth adding once the gateway is deployed separately and `chat --gateway` actually runs Hello |
| Golden `.oms` corpus replay test | **Adopt as a test with backlog item B5** | Ordinary `cargo nextest`; no new CI job |
| Dead-export check for `bounds::validate_*` | **Reject as CI** | Fix the code instead (B4) |
| Registry, `buf-action`, BSR push, `buf.lock` | **Reject** | §6–7 |
| Caching or committing generated code across jobs | **Defer** | No timings; measure first (§12) |
| `cargo deny` advisory for transitive `bincode` (RUSTSEC-2025-0141) | **Defer** | Informational; `deny.toml` has no advisories section; not checked whether advisory scanning runs elsewhere |

## 12. Schema evolution and durable data

### 12.1 Verdict: the journal stays SSE + JSON

The `.oms` journal (`journal/src/sse.rs:1-15`) is:

- raw SSE frames, with fields `event: name@rev`, `id`, `by`, `prior` (ULIDs) and `data`;
- `data` is single-line JSON of at most `DATA_HARD_CAP = 1 MiB`;
- a blank line commits each frame.

Do not migrate it to Protobuf, Avro or postcard:

1. **Replay depends on decoded meaning plus deterministic handle minting, not on bytes.**
   - GC already rewrites the bytes (`gc.rs:282`).
   - The proptest compares DOM snapshot bytes (`session/tests/replay_law.rs:669-676`).
   - The main replay hazard is handle drift from fold changes (`fold.rs:238`; `session.rs:963-1000`). That sits above the format.
2. **Torn-tail recovery is a property of the format.** It is proptested at every offset (`journal/tests/journal.rs:38-42`). A binary format would need new framing and a new proof.
3. **Payloads are mostly `RawValue` JSON** (`data.rs:515-560,750`). Protobuf would only wrap JSON in `bytes`.
4. **prost drops unknown fields (V).** Together with GC re-encoding, that would make every field addition lossy.
5. **Avro needs a second schema authority.** ADR 0003 forbids one.
6. **There is no measured cost problem.**

### 12.2 Concrete decode risks found

| ID | Risk | Evidence (OBSERVED) | Severity |
|---|---|---|---|
| R1 | **Strict `tool.result@1` on replay.** `ToolResult`'s inner `Wire` is `deny_unknown_fields`. If a newer build adds a field without bumping the rev, older builds fail `fold_tool_result` with `?`, and `Session::open` fails for the whole journal | `journal/src/data.rs:571-580`; `session/src/fold.rs:359-360`; `session.rs:258-283` | High |
| R2 | **The closed kind vocabulary fails the whole journal on open.** Any `foo@2` or new kind gives `JournalError::Frame{UnknownKind}`. Open and resume fail, and `SessionIndex::open` silently skips the file. `Genesis.version` is written as 1 and never read, so there is no "written by a newer omp" error | `journal/src/kind.rs` `is_known`; `sse.rs` `parse_frame`; `driver/src/sessions.rs:336-339`; `session/src/session.rs:229-230` | Medium (fail-closed by design; poor diagnostics) |
| R3 | **Uncapped journal reads.** `Journal::open`/`scan` call `fs::read` on the whole file. `DATA_HARD_CAP` is checked only after a frame is assembled. Only `scan_prefix` is bounded (64 KiB) | `journal/src/lib.rs` `open`/`scan`; `sse.rs:15`; `session/src/import.rs:34` | Medium for imported or relocated journals |
| R4 | **Protobuf preflight bounds are never called in production.** `validate_host_frame`/`validate_worker_frame` have only test callers. Production imports only `FRAME_MAX_BYTES` | `proto/src/bounds.rs:172-182`; `proto/tests/frame_bounds.rs`; `envd/src/worker.rs:77`; `driver/src/headless/kernel.rs:852` | Medium |
| R5 | GC prune drops unknown SSE field lines that the scanner tolerates | `journal/src/gc.rs:496-516` | Medium if extension fields ever ship |
| R6 | `processes/meta.json`: strict `version == 1` makes `load()?` fail; prost decode/re-encode loses newer fields; bytes stored as JSON number arrays | `envd/process_store.rs:202-231`; `exec.rs:626-628,1469-1478` | Low–Medium |
| R7 | Unversioned postcard BLOBs in the gateway conversation DB | `ai/src/session/store.rs:329-345` | Medium |
| R8 | tonic decode limit is implicit (4 MiB default, V) | `app/src/daemon.rs:346-358,396-402` | Low (explicitness) |
| R9 | Workspace snapshot records are written without fsync | `workspace/operations.rs:905-921` | Low |
| R10 | Dead `cache::backend` (transcript-v4 era; dangling `crate::index::SessionIndex` doc link) | `cache/src/backend.rs:195`; `serve/src/auth.rs:986` | Low |
| — | No checked-in `.oms` fixture corpus; every compatibility test builds journals with the current writer | `find -name '*.oms'` empty; `fixtures/` holds only `llm-oracle` | Gap |

### 12.3 Classification

| Item | Class | Action |
|---|---|---|
| SSE+JSON `.oms`, SHA-256 CAS, `{h,n}` BlobRef, `Hash32` hex | **Preserve** | — |
| Closed `patch@1` op set with open `Tag`/`PropKey` | **Preserve** | — |
| Strict J4 DOM-prop payloads (decoded `.ok()` with text fallback) | **Preserve** | — |
| Cache crate stores C1–C5; catalog postcard header | **Preserve** | — |
| R1 `ToolResult::Wire` | **Optimize** | Backlog B1 |
| R2 kind vocabulary diagnostics | **Optimize** | Backlog B2 |
| R3 uncapped reads | **Optimize** | Backlog B3 |
| R4 unwired preflight | **Optimize** | Backlog B4 |
| Golden `.oms` corpus | **Optimize** | Backlog B5 |
| R7 postcard DB version | **Optimize** | Backlog B6 |
| R8 tonic limits | **Optimize** | Backlog B7 |
| R10 `cache::backend` | **Optimize (delete)** | Backlog B8 |
| R5 GC drops unknown SSE lines | **Defer** | Fix if an SSE extension field is ever introduced (keep raw `Frame.span`, `sse.rs:19-23`) |
| R6 process store | **Defer** | If touched: base64 bytes, version treated as "≤ supported" |
| R9 snapshot fsync | **Defer** | Records are skipped or rebuilt on corruption |
| `transcript_v4_json` field name | **Defer** | Rename together with the next collab revision |
| Journal → Protobuf/Avro/postcard; any format migration | **Reject** | §12.1 |

### 12.4 Decode-hardening implementation backlog (not CI)

These are separate implementation items. None of them changes a format, and each lands with its own tests.

- **B1: tolerate unknown fields in `tool.result@1`.**
  - Remove `deny_unknown_fields` from `ToolResult::Wire` (`journal/src/data.rs:571-580`).
  - Keep the exactly-one-of `outcome`/`fault` check.
  - Add a unit test that a payload with an extra field decodes.
  - Alternative if the owner prefers strictness: a doc comment requiring `tool.result@2` for any field addition, plus a test asserting that every struct decoded with `?` in `session/src/fold.rs` is lenient or carries that comment.
- **B2: typed "newer journal" error.**
  - Keep the fail-closed `UnknownKind`.
  - Map an unknown kind name or `rev > known` to a typed error such as `JournalError::NewerRevision{kind}` that `open`/`resume` report to the user, instead of the index silently skipping the file.
  - Then either read and validate `Genesis.version` or delete the field (it is written at `session.rs:229-230` and never read).
- **B3: bound journal reads.**
  - Give `Journal::open`/`scan` a byte cap, or stream the decode as `scan_prefix` does.
  - Fail any frame that has gone `DATA_HARD_CAP` bytes without a terminator, before buffering further.
  - Applies at minimum to imported or relocated `.oms` paths (`kernel.rs:2658-2660`, `--resume <path>`).
- **B4: wire or delete the protobuf preflight.**
  - Call `bounds::validate_host_frame`/`validate_worker_frame` before `prost::decode` on the ext-host/worker paths that already import `FRAME_MAX_BYTES` (`envd/src/worker.rs:77`, `driver/src/headless/kernel.rs:852`).
  - Otherwise delete them and `tests/frame_bounds.rs`, following AGENTS.md's "delete dead weight".
- **B5: golden `.oms` corpus.**
  - Check in journals produced at each kind or field addition (under `fixtures/journal/`).
  - Add a replay test asserting they open and fold to the recorded snapshot.
  - This is the only way to pin old bytes against a new reader.
- **B6: version the postcard conversation DB.** Add `PRAGMA user_version` or a `MAGIC`+`SCHEMA_VERSION` header to `SqliteConversationStore` (`ai/src/session/store.rs:329-345`), following `catalog/src/snapshot.rs:30-33,406-416`.
- **B7: explicit tonic limits.** Set `max_decoding_message_size`/`max_encoding_message_size` in `app/src/daemon.rs:346-358,396-402`.
- **B8: delete `cache::backend`** together with `cache/tests/backend_protocol.rs`.
- **B9: fix the documentation drift found:**
  - `limits.py:39` `SCHEMA_REV = 7`;
  - `processes.md` naming `WorkerProcess::spawn`;
  - the `build.rs:38-41` rerun comment;
  - missing `WorkerHello` validation, recorded as an issue;
  - `chat --gateway` not running the gateway Hello, recorded as an issue.

## 13. Security

Cited limits (OBSERVED):

| Boundary | Limit | Location |
|---|---|---|
| Journal `data` per frame | 1 MiB | `journal/src/sse.rs:15` |
| Journal file | **No cap** (R3) | `journal/src/lib.rs` |
| Import provenance | 64 KiB | `session/src/import.rs:34` |
| DOM snapshot (untrusted) | high water 1,048,576 + structure validation | `dom/src/snapshot.rs:6-9` |
| Collab | frame 1 MiB, field 512 KiB, 4096 delimited, 1024 repeated, depth 16; snapshot 256 KiB × 256 | `collab/codec.rs:20-30`; `driver/src/collab/session.rs:36-39` |
| Protobuf preflight | 64 MiB, depth 32, 4096 / 1024; TML 256 KiB, depth 64 (**unwired**, R4) | `proto/src/bounds.rs:11-43` |
| envd / env frames | 64 MiB varint-length cap | `envd/src/server.rs:187`; `env/src/windows.rs:43`; `env/src/client.rs:4735` |
| docserver | 64 MiB frame, 128 sockets, 64 in flight | `docserver/wire.rs:14`; `daemon.rs:53`; `connection.rs:52` |
| LSP/DAP | 8 KiB / 16 MiB default, 64 KiB / 64 MiB hard | `docserver/lsp_process.rs:50-58`; `dap_protocol.rs:31-32` |
| RPC NDJSON | 1 MiB lines, 64 MiB reassembled | `rpc/src/framing.rs:17-19` |
| AI transports | 16 MiB | `ai/src/transport/frame.rs:16` |
| tonic daemon | implicit 4 MiB (V) | `app/src/daemon.rs` |
| JSON depth | 128 (V); `unbounded_depth` off | `Cargo.toml:216` |
| prost recursion | 100 (T) | — |

Other security points:

- Vendored schemas are pinned by SHA-256 or manifest, and the buf binary by checksum.
- `buf breaking` with a directory baseline needs no token or network beyond `origin`.
- No format change is proposed. Any future zero-copy format must use validated access only (`rkyv::access`, FlatBuffers `Verifier`; see RUSTSEC-2021-0122).
- REAPI auth differs by provider (header key, mTLS, JWT, credential helper). Keep keys out of argv (ADR 0040 `:418,1007-1011`).
- Untrusted-input paths that still lack a cap or preflight: R3 (journal import) and R4 (ext-host protobuf).

## 14. Benchmark plans (all UNVERIFIED; nothing measured)

### 14.1 Serialization

Workloads, using real OMP types:

| Workload | Real type | Formats compared |
|---|---|---|
| W1 agent event | `thread::v1::Item` / `KernelEvent` projection | prost, postcard, serde_json, ciborium, rmp-serde |
| W2 tool invocation | `env.v1.InvokeTool` (`env.proto:142`), journal `tool.result@1` | prost, JSON, postcard |
| W3 env state | `ServerHello`, `WorkspaceSnapshotList` | prost, postcard, rkyv (validated), FlatBuffers (verified) |
| W4 exec output | `ExecRequest` (`env.proto:330`), streamed `bytes` | prost `Bytes`, postcard, raw framing |
| W5 journal | `journal::Entry` → SSE | SSE+JSON baseline, JSON+zstd, prost, postcard |

Metrics:

- encode and decode ns/op;
- encoded bytes, and bytes after zstd level 3;
- allocations/op (counting global allocator) and peak RSS on replay;
- UDS round-trip p50/p99 via `encode_length_delimited`;
- append MB/s with fsync off and on;
- cold replay time;
- `bounds.rs` preflight cost, measured separately.

Harness: `crates/proto/benches/wire.rs` with `criterion` 0.7 (V) added to root `[workspace.dependencies]`, and only once work is approved.

- Inputs come from recorded fixtures.
- Every format must round-trip equal before it is timed.
- No benchmark runs in shared CI.

Expectations to test, not assume: prost and postcard are close on W1–W3, and W5's limit is fsync and I/O.

### 14.2 Proto generation

Arms:

- Arm 1, cargo + `build.rs` + protox: the baseline.
- Arm 7, ADR 0040's coarse REAPI action: only where it applies.
- protoc, `buf generate` and the Bazel arms: not applicable (§8).

Scenarios:

- S1 cold build;
- S2 warm no-op;
- S3 leaf edit to `blob.proto`, plus a comment-only variant;
- S4 shared edit to `inference/v1/common.proto`;
- S5 unrelated `.rs` edit;
- S6 generator bump;
- S7 `tonic` feature on and off;
- S8 worktree switch on a shared target directory.

Commands: `cargo build -p omp-proto --timings`, `cargo build -p omp-ai --timings`, `cargo build -p omp-app --bin omp`, `just test-pkg omp-env`.

Metrics:

- wall and CPU time;
- build-script time and `omp-proto` rustc time;
- dirty units;
- `OUT_DIR` bytes;
- dependents rebuilt.

Run at least 3 repetitions on the M1 Pro and Linux reference hosts (ADR 0040 `:1308-1314`).

**Decision rule.** Revisit splitting `omp-proto` per package only if both hold:

- the build script plus `omp-proto` rustc take more than about 5% of S3 wall time; and
- proto edits are a meaningful share of commits.

## 15. Cross-language codegen matrix (only languages actually needed)

| Language | Need | Protobuf codegen | Evidence |
|---|---|---|---|
| Rust | All owned protobuf boundaries, the vendor oracle and future REAPI | protox + prost/tonic in `build.rs` (keep) | OBSERVED §1.4 |
| Python | Extension and worker API | **None.** Python reaches the protocol through PyO3 bindings re-exporting `omp_env::frame` and `omp_env::SCHEMA_REV` compiled into the same binary | `py/src/bindings.rs:4089` [INFERENCE: no Python protobuf runtime is needed] |
| TypeScript (collab-web) | Collab browser client | **None.** The wire is hand-mapped JSON | `collab/codec.rs` |
| Tern / editors / MCP / LSP | External peers | **None.** JSON protocols (TSP, ACP, MCP, LSP/DAP) | §2 |
| Any language (third-party `ReadyPing` targets) | Must answer `HostFrame{Ping}` with `WorkerFrame{Pong}` | Not generated by OMP²; protected by keeping `toolhost` in breaking scope | `envd/exec.rs:3811-3830` [INFERENCE] |

No second language needs generated protobuf code today. Go, Java, .NET and others are out of scope.

## 16. Gates 1–8

No repository-wide gate numbering was found (grep over `docs/`). These gates are defined here for this research and ADR 0048.

| Gate | Criterion | Status |
|---|---|---|
| 1 | Inventory complete: owned, vendored and REAPI schemas; consumers; serialization crates | **Passed** (§1, §3) |
| 2 | Every boundary classified by encoding, schema, semantics, transport and whether its ends can differ | **Passed** (§2). Medium confidence on the document, env Hello and `meta.json` cross-build claims (from code and README, not run) |
| 3 | The compatibility mechanism enforced for each cross-build protobuf boundary | **Open.** Needs §11.2 landed and a first local `buf breaking` run passing on `omp2` |
| 4 | Durable data decode-safe across versions | **Open.** Backlog B1–B3, B5, B6 |
| 5 | Untrusted-input bounds active on every external decode path | **Open.** B3, B4, B7 |
| 6 | Any format or codegen change backed by measurement | **Not triggered.** No change proposed; §14 plans UNVERIFIED |
| 7 | REAPI contract and bindings decided consistently with ADR 0040 | **Passed for the decision** (§10). Implementation is deferred with ADR 0040 slice 1 |
| 8 | Owner review of ADR 0048 | **Pending** |

## 17. Confidence

- **High:** schema inventory, codegen flow, versions, CI graph, handshake constants, journal and durable formats, the security constants, upstream REAPI names, and Buf cache/generate semantics.
- **Medium:**
  - cross-build behavior of the docserver, env Hello and `meta.json`;
  - provider auth and cancellation details obtained only from search synthesis (Buildbarn, BuildGrid, NativeLink);
  - `buf breaking` reading `breaking:` from the input module (T).
- **Low:**
  - whether policy IR is journaled;
  - the `WorkerHello` path;
  - whether REAPI v2.13 is a tagged release;
  - the REAPI commit inside `bazel-remote-apis` 0.30.
