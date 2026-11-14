# 0069. Generated Go and Python gRPC clients: a protox schema bundle first, clients only after the owner rules on ADR 0048's revisit trigger

Status: Proposed 2026-11-14; EGM round 1 resolved, rounds 2-3 pending
Date: 2026-11-14 (first draft 2026-11-13)
Area: interface

Research: [docs/research/rpc-clients.md](../research/rpc-clients.md) (Gap 18 of the upstream gap closure). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2 9cb6fd6389`. Citations: `upstream:` is the OMPv1 pin; bare paths are omp2. Read only; no build, test or generator was run for this revision. This record states decisions; the note holds the evidence.

## Context

### Two protocols, not one

omp2 has two programmatic surfaces, and they must not be confused.

- **`omp rpc`, the stdio embedding protocol.** Newline-delimited JSON, one object per line, with protocol-v2 chunk reassembly; 1 MiB per physical line and 64 MiB per reassembled payload (`crates/rpc/src/framing.rs:1-2`, `crates/rpc/src/framing.rs:15-21`; the stdio actor imports `JsonLineDecoder` and `RpcFrameDecoder`, `crates/app/src/rpc_mode.rs:34`). It is not Content-Length framed: the `ContentLengthDecoder` in the same module (`crates/rpc/src/framing.rs:159-164`) is not used by `omp rpc`, and commit `0f17146ad` corrected the two texts that said otherwise (`crates/rpc/README.md:3`, `crates/rpc/README.md:8`, `crates/app/src/cli.rs:1127`). Requests are serde structs with a `type` discriminator and flattened, free-form `serde_json::Value` params (`crates/rpc/src/protocol.rs:115-125`, `crates/rpc/src/protocol.rs:1-6`). There is no schema file. This protocol drives an agent session: prompt, steer, model and session commands.
- **gRPC, from the `.proto` files.** Protobuf over HTTP/2 on a Unix socket or TCP. The daemon (`omp serve --endpoint <socket>`, `crates/app/src/cli.rs:1107-1108`, `crates/app/src/cli.rs:1666-1672`) serves model access, credentials and blobs, not agent turns. `SessionControl` is served by the session host on a different socket. This is the surface the gap asks for clients of.

### What OMPv1 does

- Typed Go, Python and Rust SDKs under `sdk/`, each an `omp-rpc` package, for one protocol: `omp --mode rpc`, JSON lines over stdio (`upstream:sdk/go/omp-rpc/README.md:3`; framing `upstream:packages/coding-agent/src/modes/rpc/rpc-frame.ts:5-8`, the same 1 MiB and 64 MiB caps as omp2).
- The typed surface is generated from a JSON Schema (`upstream:packages/coding-agent/src/modes/rpc/wire/rpc-wire.schema.json`, 74 commands under `x-rpc.commands` from line 11) by `bun run gen:rpc` (`upstream:sdk/go/omp-rpc/README.md:15-20`, `upstream:package.json:161`); `wire.go` is marked generated (`upstream:sdk/go/omp-rpc/wire.go:1`). A hand-written client does correlation, protocol-v2 negotiation and chunk reassembly (`upstream:sdk/go/omp-rpc/README.md:8-13`).
- Upstream has no proto-generated clients. Its SDKs speak the protocol that corresponds to omp2's `omp rpc`, not omp2's gRPC.
- Upstream's SDK tests do not run against a real server by default. The Python suite starts a scripted fake server written in Python (`upstream:sdk/python/omp-rpc/tests/test_client.py:1142-1149`, `RpcClient(command=[sys.executable, "-u", "-c", server])`); the Go unit tests use an in-memory fake (`upstream:sdk/go/omp-rpc/fake_test.go:21-26`). Only the Go smoke tests reach a real server, and that server is upstream's TypeScript CLI under Bun with a `PI_*` variable and a Bun fake model (`upstream:sdk/go/omp-rpc/smoke_test.go:21-33`, `upstream:sdk/go/omp-rpc/smoke_test.go:71-73`, `upstream:sdk/go/omp-rpc/smoke_test.go:208`).

### What omp2 has

- **Proto inventory.** 19 files in 15 packages under `crates/proto/proto/omp/`. Six services, 63 RPCs, counted from the `rpc` lines of the service blocks:

| Service | File | RPCs | Shapes | Served by |
|---|---|---|---|---|
| `Gateway` | `crates/proto/proto/omp/gateway/v1/gateway.proto:21-25` | 1 | 1 unary (`Hello`) | daemon |
| `ForwardProxy` | `crates/proto/proto/omp/gateway/v1/gateway.proto:29-34` | 1 | 1 server stream | daemon |
| `Auth` | `crates/proto/proto/omp/auth/v1/auth.proto:38-98` | 21 | 20 unary, 1 server stream (`WatchCredentials`) | daemon |
| `Blob` | `crates/proto/proto/omp/blob/v1/blob.proto:13-25` | 4 | 2 unary, 1 server stream (`Get`), 1 client stream (`Put`) | daemon |
| `Inference` | `crates/proto/proto/omp/inference/v1/inference.proto:39-114` | 30 | 23 unary, 5 server stream, 2 bidirectional (`Turn`, `Realtime`) | daemon |
| `SessionControl` | `crates/proto/proto/omp/session/v1/session.proto:16-38` | 6 | 5 unary, 1 server stream (`Attach`) | session host |

- **Served surface.** The daemon registers `Gateway`, `ForwardProxy`, `Inference`, `Blob` and `Auth`, 57 RPCs (`crates/app/src/daemon.rs:403-408`). The session host serves only `SessionControl`, on an owner-only socket under the data directory (`crates/driver/src/session_wire.rs:311-331`); `omp attach` is its client (`crates/app/src/attach_cmd.rs:33-39`). The session host registers no `Gateway`, although `session.proto` says negotiation uses `Gateway.Hello` with capability `session.control.v1` (`crates/proto/proto/omp/session/v1/session.proto:11-12`).
- **Daemon transport.** Owner-only Unix socket, or TCP that requires a bearer token file (`crates/app/src/daemon.rs:389-391`, `crates/app/src/daemon.rs:416-418`, `crates/app/src/daemon.rs:177`); TLS and mutual TLS in `omp-rpc` (`crates/rpc/src/tls.rs:9-18`). Every daemon service caps decode and encode at 64 MiB (`crates/app/src/daemon.rs:55-63`, `crates/proto/src/bounds.rs:11`).
- **Handshake.** `HelloRequest` carries the client's schema revision; `HelloResponse` carries the server's revision and its minimum (`crates/proto/proto/omp/gateway/v1/gateway.proto:43-63`). The contract has two rules. The client must abort when the server's revision is lower than its own (`gateway.proto:55-56`), and a client older than the server's minimum is unsupported. The Rust client applies both (`crates/rpc/src/hello.rs:101-119`) and tests the first (`crates/rpc/src/hello.rs:190`, `rejects_server_older_than_client`). Today `SCHEMA_REV` is 29 (`crates/proto/src/lib.rs:51`) and the gateway minimum is 1 (`crates/rpc/src/hello.rs:12`). The daemon advertises `auth`, `inference.native` and `gateway.forward` (`crates/app/src/daemon.rs:380-386`).
- **Build.** `build.rs` compiles every `.proto` with the pure-Rust `protox` 0.9.1 and `tonic-prost-build`, into `OUT_DIR`, never committed (`crates/proto/build.rs:1-6`, `Cargo.toml:205`). `protox`'s own CLI writes only a descriptor set; it has no plugin support (`protox-0.9.1/src/main.rs:8-33` in the Cargo registry). `prost-types` 0.14.4, already a build dependency of `omp-proto` (`crates/proto/Cargo.toml`), carries the protoc plugin messages `CodeGeneratorRequest`/`CodeGeneratorResponse` (`prost-types-0.14.4/src/compiler.rs:20`), and `protox` serves `google/protobuf/compiler/plugin.proto` from its embedded set (`protox-0.9.1/src/file/google.rs:35`).
- **Checks.** CI pins buf 1.73.0 (`.github/workflows/ci.yml:178-201`) and runs `buf format`, `buf lint` and `buf breaking` against the PR base (`.github/workflows/ci.yml:203-229`). `buf.yaml` uses WIRE and ignores `control`, `telemetry`, `ui` and `collab` (`crates/proto/buf.yaml`).
- **Package cycle.** `omp.thread.v1` imports `inference/v1/common.proto`, and `omp.inference.v1` imports `thread/v1/thread.proto` (`crates/proto/proto/omp/thread/v1/thread.proto:5`, `crates/proto/proto/omp/inference/v1/inference.proto:9`); buf's `PACKAGE_NO_IMPORT_CYCLE` is excepted for it (`crates/proto/buf.yaml`). No file sets `go_package`. There is no Go code in the workspace.
- **A scripted daemon for tests exists.** `DaemonHandle::start_for_test` (`crates/app/src/daemon.rs:327`) starts the production daemon over a `FakeProvider` registry; P7 runs the real `omp` binary against it with `--gateway <socket>` (`crates/e2e/tests/p7_tui.rs:321-334`, `crates/e2e/tests/p7_tui.rs:1069-1077`; flag at `crates/app/src/cli.rs:1869-1871`, shared by `omp rpc` through `RpcArgs.launch`, `crates/app/src/cli.rs:2114-2117`).

### Command overlap, recounted

Method: upstream's command names are the `name` fields of `x-rpc.commands` (74). omp2's names are the string patterns of the top-level request `match` in the stdio actor (`crates/app/src/rpc_mode.rs:1944-2905`), plus `session_startup`, which the actor answers before that match (`crates/app/src/rpc_mode.rs:156-170`; `crates/rpc/src/protocol.rs:111`). The count is by name; field shapes were not compared (UNVERIFIED).

- **47 upstream commands are handled by name in omp2.**
- **27 are upstream-only:** `remove_queued_message`, `promote_queued_message`, `abort_and_restore_queue`, `set_slow_mode`, `goal`, `set_ask_dialog`, `cancel_subagent`, `steer_subagent`, `live_start`, `live_connect`, `live_stop`, `live_mute`, `dictation_start`, `dictation_stop`, `set_cache_warming`, `get_session_lease`, `fork`, `navigate_tree`, `abort_branch_summary`, `set_label`, `get_logout_accounts`, `logout`, `predict_word`, `predict_word_feedback`, `btw`, `btw_cancel`, `get_btw_history`.
- **omp2's match handles 11 names that are not upstream commands.** Three of them are frames upstream sends as non-command inbound messages, not commands: `extension_ui_response`, `host_tool_update`, `host_tool_result` (`RpcInbound` in `upstream:packages/coding-agent/src/modes/rpc/wire/rpc-wire.schema.json:5941`). One is omp2's startup frame, `session_startup`. Seven are verbs only omp2 has: `approve`, `cancel`, `interrupt`, `pause`, `resume`, `quit`, `shutdown`.

The first draft said 45, 29 and 7; that count missed alternation arms such as `"interrupt" | "abort"` and `"get_entries" | "get_tree"`.

**Against the gRPC surface.** None of the 74 JSON commands is a gRPC RPC. The 57 daemon RPCs are inference, credential and blob operations that no upstream SDK command covers. The only overlap is `SessionControl.Submit`, whose `ControlCommand` carries seven verbs the proto documents as equivalent to rpc-mode verbs: prompt, steer, queue, interrupt, abort_tools, pause and cancel (`crates/proto/proto/omp/session/v1/session.proto:173-188`). So a gRPC client of the daemon does not replace upstream's agent SDK. The agent-driving role maps, if anywhere, to `SessionControl`, which only the session host serves.

### Rules that bind

- **ADR 0048 is accepted and stays as written** (accepted 2026-10-28, `docs/adr/0048-schema-and-serialization-boundaries.md:4`):
  - Decision 2 keeps git-versioned schemas with no registry, and says when to revisit: "independently released external SDKs consume these `.proto` files (then consider the BSR); or a broker event bus appears" (`docs/adr/0048-schema-and-serialization-boundaries.md:61-68`). Gap 18 is exactly that first trigger.
  - Decision 3 keeps Buf to checks and rejects `buf generate`, "BSR remote plugins and generated SDKs", `buf.lock` and `bufbuild/buf-action` (`docs/adr/0048-schema-and-serialization-boundaries.md:70-77`).
  - Decision 5 rejects Bazel and BuildBuddy for proto generation (`docs/adr/0048-schema-and-serialization-boundaries.md:144-151`).
  - Generated code is never committed, and there is no system `protoc` (`docs/adr/0048-schema-and-serialization-boundaries.md:12`).
- **Protobuf means `protox`; there is no system `protoc`** (`AGENTS.md:654`).
- **No new language runtimes.** A Go or Python client is a separate artifact for outside users, not a runtime inside omp. A Go toolchain in CI is still new to this repository.
- **Additive only.** No existing proto, service, convar, command or CLI name changes.
- A gap ADR records; it does not edit other ADRs.

## Decision

**D1. The schema bundle is built by `protox`.** A new bin `omp-proto-bundle` in `crates/proto` compiles the 19 files with `protox::compile`, as `build.rs` already does (`crates/proto/build.rs:46`). It writes a deterministic archive: the `.proto` sources, `buf.yaml`, `omp.binpb` (a self-contained `FileDescriptorSet` with imports and source info), and `manifest.json` with `schema_rev` (`SCHEMA_REV`), `gateway_min_schema_rev` (`MIN_SCHEMA_REV`), the git revision and the service and RPC list. The archive has sorted entries and no timestamps. No `protoc`, no `buf generate` and no new generator. This is a snapshot of the git-versioned schema, which ADR 0048 Decision 2 already accepts; it is not a registry.

**D2. ADR 0048 is respected, not reopened.** Publishing independently released Go or Python SDKs (S3, S5) fires ADR 0048 Decision 2's first revisit trigger as written: "independently released external SDKs consume these `.proto` files (then consider the BSR)". This record does not answer that revisit and does not amend ADR 0048. Its design uses nothing Decision 3 or 5 rejects: no `buf generate`, no BSR remote plugins or BSR-generated SDKs, no `buf.lock`, no Bazel, no committed generated code, no `protoc`. S0, S1, S2 and S4 publish no SDK, so they do not fire the trigger. S3 and S5 do not publish until the owner has recorded the Decision 2 revisit (O1). The stale sentence in ADR 0048, which says CI does not run `buf breaking` (`docs/adr/0048-schema-and-serialization-boundaries.md:29`; CI does, `.github/workflows/ci.yml:219-229`), is reported to the owner (O8), not edited here.

**D3. Python first, Go second, the daemon's five services first.** Scope is `Gateway`, `ForwardProxy`, `Auth`, `Blob` and `Inference`, the 57 RPCs the daemon serves. `SessionControl` waits for O4. Each client is a thin hand-written layer over generated or descriptor-built bindings, and provides:
- a Unix-socket channel helper, plus TCP with bearer-token and TLS credentials;
- the `Hello` handshake with both contract rules: abort with a typed error when the server's `schema_rev` is lower than the client's (`gateway.proto:55-56`), and abort with a typed error when the client's revision is lower than the server's `min_schema_rev`. Both comparisons use the revision the client was generated at. The Rust client compares the second rule against the crate constant instead (`crates/rpc/src/hello.rs:109`); that is not a model for generated clients;
- receive and send limits raised to the daemon's 64 MiB.

**D4. The Go package cycle is solved at generation time.** `thread` and `inference` map to one Go import path through plugin `M` parameters passed by S4's host, with no `.proto` edit. Whether `protoc-gen-go` accepts two proto packages in one Go package is UNVERIFIED (O3). If it does not, breaking the cycle is a schema change and gets its own decision.

**D5. One conformance scenario list, with a Rust reference run against the real binary.** The list lives in `clients/conformance/scenarios.toml`, and every client runs it against `omp serve --endpoint <socket> --data-dir <scratch>`. Each scenario needs no provider credential:
1. Hello returns `auth`, `inference.native` and `gateway.forward`.
2. One unary `Auth` call (`ListCredentials` on an empty data directory).
3. `Blob.Put` (client stream) followed by `Blob.Get` (server stream) round-trips the bytes.
4. One unary `Inference` call (`ListProviders`).
5. A 32 MiB blob in one message is accepted. That is above the gRPC default receive limit of 4 MiB [INFERENCE, not read here], so it proves the client raised its limit.
6. A server older than the client aborts: the client declares `SCHEMA_REV + 1` through a test-only override and must fail with its typed "server too old" error before any other RPC.
7. A client older than the server's minimum aborts: the client declares revision 0 against a minimum of 1 and must fail with its typed "client unsupported" error.
8. A server newer than the client is accepted: the client declares `SCHEMA_REV - 1` and the handshake succeeds.

The Rust reference run is the e2e proof `crates/e2e/tests/grpc_conformance.rs`. It runs the same list with the generated tonic clients against the `omp` binary, so the daemon side of every scenario is gated in CI even when the Python and Go jobs do not run. Each client carries the `SCHEMA_REV` it was generated at, and its release fails when that differs from the bundle's manifest.

**D6. Names and gates.** All Rust code for this ADR sits behind `cap-rpc-clients`, a new Cargo feature on `omp-proto`. It is default-off and is not forwarded to `omp-app`, so the `omp` binary and every default build are unchanged. The feature gates the `omp-proto-bundle` and `omp-proto-gen` bins through `required-features`, and with them `protox` as a runtime dependency. It is a `cap-*` feature because it adds build weight; it changes no behaviour of `omp`, so no convar is needed. The Python package is `omp-grpc` (import `omp_grpc`) under `clients/python/omp-grpc`, managed with `uv`. Go lives under `clients/go`. Both are built only by `workflow_dispatch` CI jobs. No `pi` identity anywhere, and no harness sets a `PI_*` variable. The new Rust errors follow ADR 0035 (`docs/adr/0035-rust-for-the-engine.md:55-56`): `thiserror` enums with typed `#[source]` causes. A plugin's free-text `error` field is printed once at the bin's `miette` boundary, and the typed error carries only the plugin path.

**D7. Code generation is hosted on `protox`, never `protoc`.** A new bin `omp-proto-gen` compiles the bundle's files with `protox`, builds a `prost_types::compiler::CodeGeneratorRequest` (files in dependency order, `file_to_generate` = the 19 `omp` files, the plugin parameter as given), and runs one named protoc-plugin executable on stdin and stdout. This is the standard plugin protocol of `plugin.proto`. The bin writes the `CodeGeneratorResponse` files under an output directory outside the tree, and refuses any response path that is absolute or escapes that directory. Go uses `protoc-gen-go` and `protoc-gen-go-grpc` through it. Python takes a different path, because Python message generation is built into `protoc` itself and `grpcio-tools` embeds `protoc` [INFERENCE from general knowledge; UNVERIFIED here]. `omp-grpc` therefore ships the bundle's `omp.binpb` and builds message classes at import time with the `protobuf` runtime's descriptor pool and message factory. Its method stubs are built from the service descriptors with grpcio's generic channel methods. Whether that untyped-at-edit-time shape is acceptable is O9.

**D8. The stdio protocol gets its own probe, with an omp2-owned harness.** Upstream's suites cannot be reused as they are: they exercise fakes, or upstream's own Bun server (see Context). S0 instead drives upstream's SDK clients from the upstream pin against the real `omp rpc` binary, backed by a scripted daemon, and records a result for each of the 74 commands. If the 47 shared commands pass, the owner can adopt upstream's MIT SDKs for `omp rpc` (O5). That would be a dependency on upstream's packages, not a port: nothing would be renamed or copied into omp2. No schema work is needed for that path.

## Rejected options

| Option | Why not |
|---|---|
| Do nothing | Users can already point their own tools at `crates/proto/proto`, but nobody can find the contract or its revision. |
| Check generated clients into the repository | ADR 0048: generated code is never committed (`docs/adr/0048-schema-and-serialization-boundaries.md:12`). |
| `buf generate` with local plugins | ADR 0048 Decision 3 rejects it (`:74`). D7 does the same job on the compiler the build already uses. |
| `protoc` or a `protoc`-embedding generator | AGENTS.md: no system `protoc` (`AGENTS.md:654`); the bundle and every generation step start from `protox`. |
| BSR-generated SDKs | ADR 0048 Decision 3 rejects them (`:75`); the BSR question is the owner's Decision 2 revisit, not this record's. |
| Run upstream's SDK test suites as the S0 probe | They test fakes, or upstream's own server under Bun with a `PI_*` variable; they say nothing about omp2. |
| Port upstream's JSON-Schema generator to `omp rpc` | It first needs a schema for a protocol whose params are free-form `Value`; very high cost. Superseded for the common case by D8. |
| Generate stdio clients from Rust types with `schemars` | High cost, and the open payloads stay untyped. |
| Add `go_package` to the `.proto` files | It changes shared sources for a downstream need; D4 maps at generation time instead. |

## Slices

Each slice is one PR. Tests are written first and not edited to pass. No slice changes a journal kind or payload, so no golden-journal fixture applies. Every slice leaves `just lint`, `just test` and `just check-minimal-caps` green.

**S0. Stdio SDK probe with its own harness.** Depends on nothing.
- Scope: `crates/e2e/tests/rpc_sdk_probe.rs`, `#[ignore]` by default, plus driver scripts `scripts/rpc-sdk-probe/{probe.py,probe.go}`. The test lifts P7's scripted daemon (`crates/e2e/tests/p7_tui.rs:300-345`) into `crates/e2e/src/support` and starts it. It then runs each driver with the path of upstream's SDK, taken from `OMP_RPC_PROBE_SDK`; the SDK is extracted from the pin with `git archive c308970263 sdk/<lang>/omp-rpc`. Each driver launches `omp rpc --gateway <socket> --model <scripted>` through the SDK's own process client (`RpcClient(command=...)` in Python, `Start(ctx, cmd)` in Go). It sends each of the 74 commands with minimal schema-valid params and writes one JSON line per command: `pass`, `fail` with the first mismatching field, or `missing` (omp2's `unknown_command`, `crates/app/src/rpc_mode.rs:2899-2905`).
- Tests: `probe_classifies_all_74_upstream_commands`; `probe_reports_unknown_command_as_missing`, which checks that `goal` is `missing`; `probe_round_trips_one_prompt_through_the_scripted_model`.
- Acceptance: a table of the 74 commands in the research note, and a yes or no on adopting upstream's SDKs for the shared commands (O5).
- Gate: `#[ignore]` plus an opt-in `workflow_dispatch` job, because it needs `uv`, Go and the upstream SDK path. Command: `cargo nextest run -p omp-e2e --test rpc_sdk_probe --run-ignored ignored-only`.

**S1. Schema bundle (`omp-proto-bundle`).** Depends on nothing.
- Scope: `cap-rpc-clients` on `omp-proto` (D6), the bin, and `just proto-bundle`.
- Tests (`crates/proto/tests/bundle.rs`, `required-features = ["cap-rpc-clients"]`): `bundle_contains_the_19_proto_files`, `descriptor_set_lists_six_services_and_63_rpcs` (adding a service forces a deliberate update), `manifest_revisions_equal_schema_rev_and_gateway_min`, `two_builds_are_byte_identical`.
- Acceptance: with the feature off, `omp-proto` builds no bin and its existing tests pass unchanged; `just check-minimal-caps` passes.
- Gate: `cap-rpc-clients`, default-off. Command: `just test-pkg omp-proto` and the same with the feature on.

**S2. Conformance scenarios and the Rust reference proof.** Depends on S1, which supplies the manifest revision.
- Scope: `clients/conformance/scenarios.toml` (D5) and `crates/e2e/tests/grpc_conformance.rs`, which spawns `omp serve` under an RAII owner with bounded waits. Adds `just e2e-grpc-conformance`, puts it in `just e2e`, and adds the `ci.yml` step.
- Tests: one per D5 scenario. The handshake scenarios assert the server-side facts each client's check depends on: `hello_reports_schema_rev_and_min_rev_1`, `hello_advertises_the_three_capabilities`. The others are `auth_list_credentials_on_empty_data_dir`, `blob_put_then_get_round_trips`, `inference_list_providers_answers`, `thirty_two_mib_message_is_accepted` and `sixty_four_mib_plus_one_is_refused_resource_exhausted`.
- Acceptance: the proof is gated in CI on Linux and macOS like the other e2e proofs.
- Gate: none needed, because it is test-only and uses only the default `omp` build.

**S3. Python client `omp-grpc`.** Depends on S1, S2, O1 and O9.
- Scope: `clients/python/omp-grpc`: descriptor-built bindings (D7), the hand-written layer (D3), and a conformance runner for `scenarios.toml`.
- Tests (pytest through `uv`, against `omp serve`): every D5 scenario, including `server_older_than_client_aborts_with_a_typed_error`, `client_older_than_min_schema_rev_aborts_with_a_typed_error` and `server_newer_than_client_is_accepted`; plus `package_revision_equals_bundle_manifest`.
- Acceptance: every scenario passes, and a wheel build fails when the package revision and the bundle disagree.
- Gate: opt-in `workflow_dispatch` job; nothing is published before O1.

**S4. Plugin host `omp-proto-gen`.** Depends on S1.
- Scope: the bin (D7), behind `cap-rpc-clients`, and a test-only fixture plugin bin.
- Tests (`crates/proto/tests/gen.rs`): `request_lists_files_in_dependency_order`, `plugin_parameter_reaches_the_plugin`, `response_files_land_under_out_dir`, `response_path_escaping_out_dir_is_refused`, `plugin_reported_error_is_a_typed_error`, `missing_plugin_is_a_typed_error`.
- Acceptance: with the feature off, nothing changes (as in S1).
- Gate: `cap-rpc-clients`.

**S5. Go client.** Depends on S2, S4, O1, O2 and O3.
- Scope: `clients/go`: `omp-proto-gen` with `protoc-gen-go` and `protoc-gen-go-grpc`, using the D4 `M` mapping; the hand-written layer; a conformance runner.
- Tests: `go build` and `go vet` pass on the generated module; `thread_and_inference_share_one_go_package`; every D5 scenario, including the three handshake scenarios; `module_revision_equals_bundle_manifest`.
- Acceptance: as S3. Go is installed only in the opt-in job.
- Gate: opt-in `workflow_dispatch` job; nothing is published before O1.

## Acceptance tests

1. S0: the probe classifies all 74 upstream commands, and the 27 upstream-only names classify as `missing`.
2. S1: the bundle holds 19 files, six services and 63 RPCs, and its manifest's revisions equal `SCHEMA_REV` and the gateway minimum. Two builds are byte-identical. With the feature off, nothing changes.
3. S2: the Rust reference proof passes every D5 scenario against the `omp serve` binary in CI.
4. S3 and S5: each client passes the same scenario list, including the server-older-than-client abort, and its release fails on a revision mismatch with the bundle.
5. S4: plugin responses cannot write outside the output directory, and plugin failures surface as typed errors.

## Upstream origin and license

Nothing is ported. S0 runs upstream's MIT SDKs from the pin as test inputs and copies nothing into the tree. If the owner adopts them for `omp rpc` (O5), they are used as published upstream packages. Upstream's `LICENSE` is MIT (copyright Mario Zechner, Can Bölük, Stencil Labs, Inc.; `upstream:LICENSE:1-5`); the Python SDK has its own MIT file (`upstream:sdk/python/omp-rpc/LICENSE:1-3`). Ideas kept from upstream are the binding rules (strict enums, an `Extra` map for open records) and the split between a generated wire layer and a hand-written client (`upstream:sdk/go/omp-rpc/README.md:5-13`, `upstream:sdk/go/omp-rpc/README.md:22-29`). `protox` is MIT or Apache-2.0 (`protox-0.9.1/Cargo.toml:42`) and is already a dependency. The licences of `protoc-gen-go`, `protoc-gen-go-grpc`, `grpcio` and `protobuf` were not checked (UNVERIFIED). `deny.toml` covers Rust crates only, so their review is manual before S3 or S5 publishes.

## Consequences

- Outside users get a versioned, language-neutral contract (S1) with no new tool, and later typed clients, once the owner has recorded the ADR 0048 revisit.
- `protoc` stays out of the repository. The one code-generation host is `protox`-based and opt-in.
- A published client is a compatibility promise for the services it covers. `buf breaking` already checks WIRE for them; four other packages are ignored, and none of those four has a service.
- Because of the server-older-than-client rule, a client generated at revision N refuses every server below N. Clients must be released per schema revision, and users must not run a client newer than their daemon.
- The stdio `omp rpc` protocol stays hand-written and untyped in its params unless S0 supports adopting upstream's SDKs.

## Open questions for the owner

- **O1. ADR 0048 Decision 2 revisit.** Publishing S3 and S5 fires the trigger "independently released external SDKs consume these `.proto` files (then consider the BSR)". How does the owner answer it: keep git-versioned schemas plus the S1 bundle, or adopt the BSR? Recommendation: keep git-versioned schemas. The bundle is the versioned artifact, and ADR 0048 Decision 3's rejections stay in force. This record also reads the trigger as not fired by publishing the bundle alone, because the sources are already public in git. The owner should confirm that reading. Until O1 is answered, S3 and S5 publish nothing.
- **O2. Go toolchain.** In this repository's CI as an opt-in job, or in a separate repository? Recommendation: an opt-in job here; move it out if Go grows beyond generation.
- **O3. Go package cycle.** Does one Go package for both cyclic proto packages generate cleanly? Not tried. Recommendation: try it in S5 before any schema change.
- **O4. `SessionControl` scope.** It is the only gRPC overlap with upstream's agent SDK (`session.proto:173-188`), but its socket registers no `Gateway`, so a client cannot perform the `Hello` that `session.proto:11-12` describes. Registering `Gateway` on the session host would change an existing server, which is outside this additive record. Recommendation: leave it out of S3 and S5; decide separately whether the session host gains `Hello`.
- **O5. Stdio clients.** Adopt upstream's MIT SDKs for `omp rpc` if S0 passes for the 47 shared commands? Recommendation: decide on S0's table, not before.
- **O6. Stability.** Which services are stable enough to promise? Recommendation: `Gateway`, `Auth` and `Blob` first; mark `Inference` experimental.
- **O7. Remote message cap.** Should there be a lower server-side cap for remote TCP before a remote client is published? Not evaluated.
- **O8. Stale text in ADR 0048.** Its Context says CI does not run `buf breaking` (`docs/adr/0048-schema-and-serialization-boundaries.md:29`), but CI does (`.github/workflows/ci.yml:219-229`). This record does not edit ADR 0048. Recommendation: the owner corrects it in an ADR 0048 amendment.
- **O9. Python binding shape.** Is a descriptor-built Python client acceptable (D7), or are statically typed generated modules required? Statically typed modules would need a generator that embeds `protoc`. Recommendation: descriptor-built, with typing stubs as later work.

## What the review changed

### Round 1 (EGM, recorded in STATUS.md row 0069; the first round's full text is not in this record)

- **Finding: state ADR 0048 Decision 2's BSR revisit trigger as written and respect 0048's locked choice, not reopen it.** Confirmed. `docs/adr/0048-schema-and-serialization-boundaries.md:65-67` names "independently released external SDKs" as the trigger, and the first draft's D2 and S2 proposed amending 0048 inside this record. Changed: D2 now quotes the trigger, stays inside Decisions 3 and 5, and does not amend 0048. The old S2 "ADR 0048 amendment" slice is removed. The revisit is O1, and it gates publication in S3 and S5. The stale `buf breaking` sentence moved to O8.
- **Finding: `protox`, not `protoc`; the schema bundle must be produced with protox.** Confirmed. `AGENTS.md:654` says "Protobuf: `protox`; no system `protoc`", and `docs/adr/0048-schema-and-serialization-boundaries.md:12` agrees. The first draft offered `protoc` in D2 and O1. Changed: D1 names `protox::compile`. D7 adds a `protox`-hosted plugin runner, because `protox`'s CLI has no plugin support (`protox-0.9.1/src/main.rs:8-33`) while `prost-types` carries `CodeGeneratorRequest` (`prost-types-0.14.4/src/compiler.rs:20`). Python avoids `protoc` by building from descriptors (O9). `protoc` now appears only in Rejected options.
- **Finding: S0 needs its own harness, because the upstream suites use fakes.** Confirmed. The Python suite runs a Python fake server (`upstream:sdk/python/omp-rpc/tests/test_client.py:1142-1149`), the Go unit tests use an in-memory fake (`upstream:sdk/go/omp-rpc/fake_test.go:21-26`), and the Go smoke tests run upstream's Bun CLI with a `PI_*` variable (`upstream:sdk/go/omp-rpc/smoke_test.go:71-73`). Changed: D8 and S0 define an omp2 harness. It drives upstream's SDK clients against the real `omp rpc` over the P7 scripted daemon (`crates/e2e/tests/p7_tui.rs:321-334`), with named tests and a gate. The Content-Length item left S0, because commit `0f17146ad` already fixed both texts (`crates/rpc/README.md:3`, `crates/app/src/cli.rs:1127`).
- **Finding: recount the command overlap, separating what the clients cover from the served gRPC surface, and cite the proto files.** Confirmed: the first count was wrong. A recount from `upstream:packages/coding-agent/src/modes/rpc/wire/rpc-wire.schema.json:11` and `crates/app/src/rpc_mode.rs:1944-2905` gives 47 shared, 27 upstream-only and 11 omp2-side names, not 45, 29 and 7. Three of the eleven are upstream inbound frames (`rpc-wire.schema.json:5941`), not commands. Changed: Context has a per-service table citing each service block, 57 daemon RPCs plus 6 `SessionControl` RPCs. It states that no JSON command is a gRPC RPC and that the only overlap is `SessionControl.Submit` (`session.proto:173-188`). That feeds O4.
- **Finding: a server-older-than-client test is missing.** Confirmed. `crates/proto/proto/omp/gateway/v1/gateway.proto:55-56` requires the client to abort when the server's revision is lower; the Rust client does (`crates/rpc/src/hello.rs:101-108`, test at `:190`); the first draft checked only the minimum. Changed: D3 states both rules. D5 adds scenarios 6 to 8 (older server aborts, client below the minimum aborts, newer server is accepted), and S3 and S5 name the tests. Consequences records the release rule this implies.

No round-1 finding was rejected. Rounds 2 and 3 (fresh-context Goldfish reviewers) are pending. This design has not been machine-reviewed after this revision and is not owner-approved.

## Status in omp

**Status: Not started.** Reusable today: the proto sources and `buf.yaml`; `protox` in `crates/proto/build.rs`; `prost-types`' plugin messages; the buf pin in CI; `Gateway.Hello` and its Rust client (`crates/rpc/src/hello.rs`); UDS and TLS plumbing in `omp-rpc`; the scripted daemon used by P7. Not yet implemented: the `cap-rpc-clients` feature, `omp-proto-bundle`, `omp-proto-gen`, `clients/`, and the `grpc_conformance` and `rpc_sdk_probe` tests.

## References

- [docs/research/rpc-clients.md](../research/rpc-clients.md); `docs/research/protocols-schemas-serialization.md`.
- ADR 0048 (schema and serialization boundaries; Decisions 2, 3, 5); ADR 0035 (errors); ADR 0049 (`cap-*` features; nothing here is a vendor integration). ADRs 0006, 0024 and 0036 were checked: no conflict, because no sandbox, roster or extension-runtime change is involved.
- `crates/proto`, `crates/rpc/src/hello.rs`, `crates/rpc/src/framing.rs`, `crates/app/src/daemon.rs`, `crates/app/src/rpc_mode.rs`, `crates/driver/src/session_wire.rs`, `crates/e2e/tests/p7_tui.rs`.
