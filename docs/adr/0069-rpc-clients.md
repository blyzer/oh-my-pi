# 0069. RPC clients: publish a protox schema bundle and probe upstream's stdio SDK now; defer generated gRPC clients until a consumer exists

Status: Proposed 2026-11-14; EGM rounds 1-2 resolved, round 3 pending
Date: 2026-11-14 (first draft 2026-11-13)
Area: interface

Research: [docs/research/rpc-clients.md](../research/rpc-clients.md) (gap 18 of the upstream gap closure, `docs/research/upstream-gap-closure-brief.md`). Upstream pin `c308970263` (v18.20.0); omp2 base `origin/omp2` as merged on this branch.

Citation conventions:
- `upstream:` is the OMPv1 pin, read with `git show c308970263:<path>`; bare paths are omp2; `registry:` is the Cargo registry source of a pinned dependency.
- EGM is the Elephant-Goldfish Model design review the brief prescribes. Goldfish reviewers are fresh-context subagents.
- BSR is the Buf Schema Registry. "The owner" is the repository owner, who approves ADRs.

Read only: no build, test or generator was run for this record.

## Context

### Two protocols, not one

- **`omp rpc`, the stdio embedding protocol.** Newline-delimited JSON, one object per line, with protocol-v2 chunk reassembly. Physical lines are capped at 1 MiB and reassembled payloads at 64 MiB (`crates/rpc/src/framing.rs:1-2`, `:15-21`; the actor imports `JsonLineDecoder` and `RpcFrameDecoder`, `crates/app/src/rpc_mode.rs:34`). It is not Content-Length framed. The `ContentLengthDecoder` in the same module (`crates/rpc/src/framing.rs:159-164`) is unused by `omp rpc`, and commit `0f17146ad` corrected the texts that said otherwise (`crates/rpc/README.md:3`, `:8`; `crates/app/src/cli.rs:1127`). Requests are serde structs with a `type` discriminator and flattened, free-form `serde_json::Value` params (`crates/rpc/src/protocol.rs:1-6`, `:115-125`). There is no schema file. This protocol drives an agent session.
- **gRPC, from the `.proto` files.** Protobuf over HTTP/2. `omp serve --endpoint <socket>` runs the daemon on a local endpoint only (`crates/app/src/cli.rs:1107-1108`, `:3923-3927`). It serves model access, credentials and blobs, not agent turns. `SessionControl` is served by a different process on a different socket (below).

### What OMPv1 does

- Typed Go, Python and Rust SDKs under `sdk/`, each an `omp-rpc` package, for one protocol: `omp --mode rpc`, JSON lines over stdio (`upstream:sdk/go/omp-rpc/README.md:3`). The framing is the same as omp2's, with the same 1 MiB and 64 MiB caps (`upstream:packages/coding-agent/src/modes/rpc/rpc-frame.ts:5-8`).
- All three SDKs are generated from one JSON Schema, which lists 74 commands under `x-rpc.commands` (`upstream:packages/coding-agent/src/modes/rpc/wire/rpc-wire.schema.json:11`). The generator is `bun run gen:rpc` (`upstream:sdk/go/omp-rpc/README.md:15-20`, `upstream:package.json:161`); the Python SDK documents the same source (`upstream:sdk/python/omp-rpc/README.md`, section "Generated from the wire schema"). A hand-written client in each SDK handles correlation and protocol-v2 negotiation (`upstream:sdk/go/omp-rpc/README.md:8-13`).
- Upstream has no proto-generated clients. Its SDKs correspond to omp2's `omp rpc`, not to omp2's gRPC.
- Upstream's SDK tests mostly exercise fakes:
  - Python runs a scripted fake server (`upstream:sdk/python/omp-rpc/tests/test_client.py:1142-1149`).
  - Go unit tests use an in-memory fake (`upstream:sdk/go/omp-rpc/fake_test.go:21-26`).
  - Only the Go smoke tests reach a real server: upstream's own Bun CLI, run with a `PI_*` variable and a Bun fake model (`upstream:sdk/go/omp-rpc/smoke_test.go:21-33`, `:71-73`, `:208`).

### What omp2 has

- **Proto inventory.** 19 files in 15 packages under `crates/proto/proto/omp/`, with 6 services and 63 RPCs. ADR 0048's Context says 18 files in 14 packages (`docs/adr/0048-schema-and-serialization-boundaries.md:12`). The difference is `omp.session.v1`, added later (commit `1f249d2fa9`).

| Service | File | RPCs | Shapes | Served by |
|---|---|---|---|---|
| `Gateway` | `crates/proto/proto/omp/gateway/v1/gateway.proto:21-25` | 1 | 1 unary (`Hello`) | daemon |
| `ForwardProxy` | `crates/proto/proto/omp/gateway/v1/gateway.proto:29-34` | 1 | 1 server stream | daemon |
| `Auth` | `crates/proto/proto/omp/auth/v1/auth.proto:38-98` | 21 | 20 unary, 1 server stream | daemon |
| `Blob` | `crates/proto/proto/omp/blob/v1/blob.proto:13-25` | 4 | 2 unary, 1 server stream, 1 client stream | daemon |
| `Inference` | `crates/proto/proto/omp/inference/v1/inference.proto:39-114` | 30 | 23 unary, 5 server stream, 2 bidirectional | daemon |
| `SessionControl` | `crates/proto/proto/omp/session/v1/session.proto:16-38` | 6 | 5 unary, 1 server stream | `omp chat` session host |

- **Served surface.**
  - The daemon registers five services, 57 RPCs in all (`crates/app/src/daemon.rs:403-408`).
  - `SessionControl` is served by the running `omp chat` process on an owner-only socket under the data directory (`crates/app/src/chat_cmd.rs:1342-1345`, `crates/driver/src/session_wire.rs:311-331`). That socket registers no `Gateway`.
- **Transport as served.**
  - `omp serve` builds only a local endpoint (`crates/app/src/cli.rs:3923-3927`).
  - TCP exists only through `omp auth-gateway serve --bind`, as plaintext HTTP/2 with a bearer token (`crates/app/src/auth_gateway_cmd.rs:33-40`); TCP without a token is refused (`crates/app/src/daemon.rs:417`).
  - `omp-rpc` has TLS helpers (`crates/rpc/src/tls.rs:9-18`), but no crate outside `crates/rpc` calls `server_tls` or `client_tls`, so the daemon serves no TLS today.
  - Every daemon service caps a single message at 64 MiB (`crates/app/src/daemon.rs:55-63`).
- **Handshake.** `HelloRequest` carries the client's schema revision; `HelloResponse` carries the server's revision and its minimum (`crates/proto/proto/omp/gateway/v1/gateway.proto:43-63`).
  - A client must abort when the server is older than it (`gateway.proto:9-13`, `:55-56`).
  - A client older than the server's minimum is unsupported.
  - The Rust client applies both rules (`crates/rpc/src/hello.rs:101-119`) and tests the first (`:190`).
  - `SCHEMA_REV` is 29 (`crates/proto/src/lib.rs:51`); the gateway minimum `MIN_SCHEMA_REV` is 1 and lives in `omp-rpc` (`crates/rpc/src/hello.rs:12`, re-exported at `crates/rpc/src/lib.rs:31`). envd has its own `MIN_SCHEMA_REV = 4` (`crates/envd/src/server/mod.rs:258`).
- **Capabilities.** `gateway.proto` says a client uses an optional surface only when both peers advertise its id, and lists `inference.turn`, `inference.invoke`, `auth`, `blob.v1`, `search` and `media` (`gateway.proto:15-18`). The daemon advertises `auth`, `inference.native` and `gateway.forward` (`crates/app/src/daemon.rs:380-386`). Only a test server advertises `blob.v1` (`crates/rpc/src/hello.rs:157`).
- **Build and checks.**
  - `build.rs` compiles every `.proto` with the pure-Rust `protox` 0.9.1 into `OUT_DIR`, never committed (`crates/proto/build.rs:1-6`, `:46`; `Cargo.toml:205`). It locates sources through the run-time `CARGO_MANIFEST_DIR`, never `env!` (`crates/proto/build.rs:26-32`).
  - CI pins buf 1.73.0 and runs `buf format`, `buf lint` and `buf breaking` (WIRE) against the PR base (`.github/workflows/ci.yml:178-229`, `crates/proto/buf.yaml`).
  - `omp-proto` does not depend on `omp-rpc`, while `omp-rpc` depends on `omp-proto` (`crates/proto/Cargo.toml`; `crates/rpc/Cargo.toml:16`).
- **CI coverage of workspace members.** Every member is linted by `cargo clippy --workspace --all-targets` (`justfile:72`) and tested by `cargo nextest run --workspace --exclude omp-e2e` (`justfile:283`). A crate that nothing links but that sits in the workspace, like `crates/roster-probe` (`crates/roster-probe/Cargo.toml:1-20`), is therefore compiled, linted and tested in CI.
- **A scripted daemon for tests.** `DaemonHandle::start_for_test` (`crates/app/src/daemon.rs:327`) runs the production daemon over a `FakeProvider` registry. P7 wraps it in a P7-local `ScriptedGateway` (`crates/e2e/tests/p7_tui.rs:234-250`, `:319-351`) and runs the `omp` binary against it with `--gateway`, `--project`, `--session` and `--approval-mode yolo` (`crates/e2e/tests/p7_tui.rs:1069-1087`). `omp rpc` shares those launch flags through `RpcArgs.launch` (`crates/app/src/cli.rs:2114-2117`, `:1869-1871`).

### Command overlap

Upstream's 74 command names, compared with the string patterns of omp2's top-level request `match` (`crates/app/src/rpc_mode.rs:1944-2911`), plus `session_startup`, which is answered before that match (`crates/app/src/rpc_mode.rs:156-170`; `crates/rpc/src/protocol.rs:111`). The comparison is by name only; field shapes were not compared (UNVERIFIED).

- **47 shared.**
- **27 upstream-only:**
  - queue: `remove_queued_message`, `promote_queued_message`, `abort_and_restore_queue`;
  - modes: `set_slow_mode`, `goal`, `set_ask_dialog`, `set_cache_warming`;
  - subagents: `cancel_subagent`, `steer_subagent`;
  - voice: `live_start`, `live_connect`, `live_stop`, `live_mute`, `dictation_start`, `dictation_stop`;
  - session: `get_session_lease`, `fork`, `navigate_tree`, `abort_branch_summary`, `set_label`;
  - accounts: `get_logout_accounts`, `logout`;
  - prediction: `predict_word`, `predict_word_feedback`;
  - btw: `btw`, `btw_cancel`, `get_btw_history`.
- **11 names on the omp2 side that are not upstream commands:**
  - three upstream inbound frames: `extension_ui_response`, `host_tool_update`, `host_tool_result` (`RpcInbound`, `upstream:packages/coding-agent/src/modes/rpc/wire/rpc-wire.schema.json:5941`);
  - `session_startup`;
  - seven omp2-only verbs: `approve`, `cancel`, `interrupt`, `pause`, `resume`, `quit`, `shutdown`.

No JSON command is a gRPC RPC. The only gRPC overlap with the agent-driving commands is `SessionControl.Submit`, whose `ControlCommand` has seven verbs. Four of them are documented as equivalent to rpc-mode verbs (prompt, steer, queue, interrupt); `abort_tools`, `pause` and `cancel` carry no equivalence note (`crates/proto/proto/omp/session/v1/session.proto:172-188`).

### What the gap asks, and who would use it

The brief's gap 18 asks for "Generated RPC clients (Go, Python) from the existing proto definitions". Neither the brief nor the research note names a consumer of a daemon client (`docs/research/rpc-clients.md`, "Question"). The parity part of the gap is upstream's SDKs, and those correspond to `omp rpc`, not to gRPC. Publishing gRPC clients would also force decisions on existing server behaviour that an additive record cannot make: the capability set, TLS and the revision model (see Deferred).

### Rules that bind

- **ADR 0048 is accepted** (`docs/adr/0048-schema-and-serialization-boundaries.md:4`).
  - Decision 2: git-versioned schemas, no registry. Its revisit trigger reads "independently released external SDKs consume these `.proto` files (then consider the BSR)" (`:61-68`).
  - Decision 3: Buf is for checks only, and `buf generate` and BSR remote plugins and generated SDKs are rejected (`:70-77`).
  - Decision 5: Bazel and BuildBuddy are rejected for proto generation (`:144-151`).
  - Generated code is never committed (`:12`).
- **Protobuf means `protox`; there is no system `protoc`** (`AGENTS.md:656`).
- **`cap-<name>` features are ADR 0049 T1 capability crates**, default-on in `omp-app` only (`docs/adr/0049-tiered-integration-architecture.md:47`, `:94-101`). A developer tool is not one.
- **Additive only.** No existing proto, service, Hello response, convar, command or CLI name changes.
- **A gap ADR records; it does not edit other ADRs.**

## Decision

**D1. Schema bundle in a standalone tool crate.** A new workspace member, `crates/proto-bundle` (package and bin `omp-proto-bundle`), depends on `omp-proto` for `SCHEMA_REV` and on `omp-rpc` for `MIN_SCHEMA_REV`. No member depends on it, so it is linked into nothing, as `crates/roster-probe` is. That placement removes the dependency cycle: `omp-proto` cannot read an `omp-rpc` constant. CI compiles, lints and tests the crate like any other member (`justfile:72`, `:283`). The crate has no Cargo feature and no convar, because it changes nothing in the `omp` binary.

The bin compiles the 19 files with `protox` at run time and takes every input as an argument:
- `--proto-root`;
- `--buf-yaml`;
- `--git-rev <rev>` and `--dirty`, which the `just` recipe supplies; the tool runs no git;
- `--out`.

It writes one directory, `omp-proto-r<SCHEMA_REV>/`, holding:
- `proto/omp/<package>/v1/*.proto`, a byte copy. The copied `buf.yaml`'s `modules: - path: proto` resolves inside the bundle.
- `buf.yaml`.
- `omp.binpb`: a self-contained `FileDescriptorSet` with imports and source info.
- `manifest.json`, with keys in this order:
  - `schema_rev`;
  - `gateway_min_schema_rev`, from `omp_rpc::MIN_SCHEMA_REV`, named so it cannot be mistaken for envd's minimum of 4;
  - `git_rev`, `git_dirty`;
  - `files`: sorted relative paths, each with a `sha256:` digest from `omp_core::Hash32`;
  - `services`: each with its RPCs and their client- and server-streaming flags, derived from the same descriptor set in the same run.

The output is a directory, not an archive: the workspace has no tar or zip crate (`Cargo.toml:61`, `:333`), and packing is a release decision (O6). Errors are a `thiserror` enum with typed `#[source]` causes (ADR 0035, `docs/adr/0035-rust-for-the-engine.md:55-56`), rendered once at the bin's `miette` boundary. This is a snapshot of the git-versioned schema, which ADR 0048 Decision 2 already accepts; it adds no registry, no `protoc` and no `buf generate`.

**D2. ADR 0048 is respected, not reopened.** This record publishes no SDK, so it does not fire ADR 0048 Decision 2's revisit trigger. It reads the bundle alone as not firing it either: the sources are already public in git, and the bundle is a copy of them (O1 asks the owner to confirm). Any later record that publishes generated clients fires the trigger as written, and must wait for the owner's answer. This record does not amend ADR 0048.

**D3. Probe upstream's stdio SDK against `omp rpc`, Python only, with an omp2 harness.** Upstream's three SDKs are generated from one schema, so the Python SDK's strict decoders test the wire for all three. The Go and Rust SDKs' hand-written clients are not probed, and omp2 already has its own Rust client (`crates/rpc/README.md:7`). The harness drives upstream's own `RpcClient` (`upstream:sdk/python/omp-rpc/tests/test_client.py:1149`, `command=[...]`) against the real `omp rpc`, backed by a scripted daemon.

Each command is classified as one of:
- **`pass`:** the SDK method returned and its strict decoder accepted the result (`upstream:sdk/go/omp-rpc/README.md:24-29`, the rules shared by all three).
- **`fail`:** the SDK raised, or omp2 answered with an error code other than `unknown_command`. The first error or mismatching field is recorded.
- **`missing`:** omp2 answered `unknown_command` (`crates/app/src/rpc_mode.rs:2903-2911`).

The result is a dated table in the research note, which feeds O5. The probe is a measurement, not a maintained gate.

**D4. Generated gRPC clients are deferred.** No Go or Python gRPC client, plugin host or conformance suite is built under this record. A later record may take them up when a consumer exists; it must answer the preconditions listed under "Deferred" and O1. Until then, S1's bundle is the language-neutral contract: anyone can generate from `omp.binpb` with their own tools.

## Rejected options

| Option | Why not |
|---|---|
| Generate Go and Python gRPC clients now (the round-1 S2-S5) | No named consumer, and they do not close the parity gap, which is upstream's stdio SDKs. Publishing them needs owner rulings on existing server behaviour (capabilities, TLS, revision model) that an additive record cannot make. Deferred under D4. |
| Put the bundle bin in `omp-proto` behind a feature | `MIN_SCHEMA_REV` is in `omp-rpc`, which depends on `omp-proto` (`crates/rpc/Cargo.toml:16`), so the bin would need a dependency cycle. A `cap-*` name would also misuse ADR 0049's T1 vocabulary (`docs/adr/0049-tiered-integration-architecture.md:47`). |
| Embed the descriptor set from `omp-proto`'s `OUT_DIR` | It would change `omp-proto`'s build output and public surface for a tool's convenience. Compiling again with the same workspace `protox` stays inside the new crate. |
| A home-grown plugin host or `buf generate` for Go | It is only needed by a deferred client. `buf generate` is rejected by ADR 0048 Decision 3 (`:74`). |
| `protoc`, or a generator that embeds it | `AGENTS.md:656`. |
| Run upstream's SDK suites as the probe | They test fakes, or upstream's Bun server with a `PI_*` variable. |
| Port upstream's JSON-Schema generator to `omp rpc` | It first needs a schema for free-form `Value` params; very high cost. |
| Add `go_package` to the `.proto` files | It changes shared sources for a deferred need. |

## Slices

Each slice is one PR. Tests are written first and are not edited to pass. Neither slice changes a journal kind or payload, so no golden-journal fixture applies. Neither changes user-visible `omp` behaviour, so neither adds a gated e2e proof. Both leave `just lint`, `just test` and `just check-minimal-caps` green.

**S1. Schema bundle crate.** Depends on nothing. Lands first, because it has no external prerequisites.
- Scope:
  - `crates/proto-bundle` (`Cargo.toml` with workspace metadata and lints, `README.md`, `src/main.rs`, `src/lib.rs`);
  - one `area/*` entry in `.github/labeler.yml`;
  - recipe `just proto-bundle out="target/proto-bundle"`, which passes `crates/proto/proto`, `crates/proto/buf.yaml`, the `git rev-parse HEAD` output and a dirty flag from `git status --porcelain`.
- Tests, in `crates/proto-bundle/tests/bundle.rs`, locating the repository through the run-time `CARGO_MANIFEST_DIR`:
  - `bundle_contains_the_19_proto_files`;
  - `descriptor_set_lists_six_services_and_63_rpcs_with_their_stream_shapes`, the table above; adding a service or RPC forces a deliberate update;
  - `manifest_revisions_come_from_omp_proto_and_omp_rpc` (`schema_rev == omp_proto::SCHEMA_REV`, `gateway_min_schema_rev == omp_rpc::MIN_SCHEMA_REV`);
  - `manifest_file_digests_match_the_copied_sources`;
  - `two_bundles_from_one_tree_are_byte_identical`;
  - `dirty_flag_is_recorded`;
  - `bundled_buf_yaml_module_path_exists`;
  - `missing_proto_root_is_a_typed_error`;
  - `uncompilable_proto_is_a_typed_compile_error`, using a temp-dir fixture.
- Acceptance: the tests pass in `just test` in CI. With the crate added, `omp-app` and every existing member build unchanged, because nothing depends on the new crate. `just check-minimal-caps` passes.
- Gate: none needed beyond "linked into nothing"; see D1.
- Command: `just test-pkg omp-proto-bundle`.

**S0. Stdio SDK probe.** Depends on nothing; independent of S1.
- Scope:
  - **Scripted daemon, moved not copied.** P7's `ScriptedGateway` start-up moves into `crates/e2e/src/support/scripted_gateway.rs`, and P7 calls it. P7 keeps its own scripts, and its behaviour does not change.
  - **Probe test.** `crates/e2e/tests/rpc_sdk_probe.rs` is `#[ignore]`d, so it compiles and lints in CI but runs only when asked.
  - **Fixture.** `scripts/rpc-sdk-probe/params.json` holds one entry per upstream command: the minimal required params, written by hand from that command's `params` definition in `rpc-wire.schema.json`, and whether the command needs a scripted model turn.
  - **Driver.** `scripts/rpc-sdk-probe/probe.py` writes one JSON line per command.
- Isolation: every command runs in its own fresh `omp rpc` process. Each process gets a fresh scratch `HOME`, `OMP_DATA_DIR`, project and session file, so session-ending or side-effecting commands (`new_session`, `switch_session`, `bash`, `export_html`, `handoff`, `compact`, `login`) cannot affect another command. Each process is bounded by a timeout and RAII-owned. The launch line is `omp rpc --gateway <socket> --model <scripted model key> --project <scratch>/project --session <scratch>/session.oms --approval-mode yolo`, as P7 does for its scratch project. Commands that need a turn get one canned text reply from the scripted daemon.
- Upstream SDK: `c308970263` is not an ancestor of omp2 (`git merge-base --is-ancestor` is false). The person running the probe fetches the pin from `github.com/can1357/oh-my-pi`, extracts `sdk/python/omp-rpc` with `git archive`, and points `OMP_RPC_PROBE_SDK` at it. `uv` resolves its dependencies over the network; nothing is committed from it.
- Tests:
  - `probe_classifies_all_74_upstream_commands`;
  - `upstream_only_commands_classify_as_missing`, which checks the 27 names above;
  - `prompt_round_trips_through_the_scripted_daemon`.
- Acceptance: a dated table of the 74 commands in the research note, listing class, error code or field, and whether the outcome is expected in a fresh session. The 7 omp2-only verbs are listed as unprobed. P7 passes unchanged.
- Gate: `#[ignore]`. Command: `OMP_RPC_PROBE_SDK=<path> cargo nextest run -p omp-e2e --test rpc_sdk_probe --run-ignored ignored-only`. There is no CI job, because the probe needs the network and an upstream checkout. Like the unwired `crates/app/tests/stock_sdk` harness, the run itself can go stale; only its compilation is kept current.

## Deferred: preconditions for a later gRPC client record

Round 2 found these facts in source. A record that builds clients must decide them first.

1. **Capabilities.** The daemon does not advertise `blob.v1`, `inference.turn` or `inference.invoke`, the ids that `gateway.proto:15-18` names (`crates/app/src/daemon.rs:380-386`). A client that follows the contract cannot use `Blob` today. Either the daemon's advertisement changes, which changes an existing Hello response, or the contract is clarified (O3).
2. **Transport.** There is no TLS server and no TCP for `omp serve`. The only TCP endpoint is plaintext bearer, and it exposes all of `Auth`, including `RevealCredential` and `MintScopedToken` (`crates/proto/proto/omp/auth/v1/auth.proto:75`, `:98`).
3. **Compatibility checks.** `buf breaking` uses WIRE because "generated Rust is never committed" (`crates/proto/buf.yaml`, breaking comment; ADR 0048 `:111-112`). Published clients break on renames that WIRE allows.
4. **Release identity.** `SCHEMA_REV` is one manual, global counter (`crates/proto/src/lib.rs:45-51`). Rev 28 to 29 touched only `env.proto` (commit `24ea21e8db`), and adding `omp.session.v1` bumped nothing (commit `1f249d2fa9`). Combined with the server-older-than-client rule, it cannot serve as a client's release identity as it stands. S1's per-file digests show what changed, but they do not decide this.
5. **Size limits.**
   - A single oversized message is refused with `OUT_OF_RANGE` by tonic's decoder (`registry:tonic-0.14.6/src/codec/decode.rs:192-195`).
   - Only the cumulative `Blob.Put` check returns `RESOURCE_EXHAUSTED` (`crates/serve/src/blob.rs:13-14`, `:142-147`).
   - No credential-free RPC sends a client a message over 4 MiB, because `Blob.Get` uses 64 KiB chunks.
6. **Reference handshake.** The Rust handshake's revision override is private (`crates/rpc/src/hello.rs:85-90`), and its second check uses the crate constant (`:109`).
7. **Coverage.** The bidirectional and most streaming RPCs need a scripted provider (`DaemonHandle::start_for_test`), not `omp serve`.
8. **Go generation.** No file sets `go_package`, so a Go generator needs an import-path mapping for all 19 files, and the `thread`/`inference` package cycle must share one Go package (UNVERIFIED).
9. **Distribution.** A Go module is fetched from VCS tags or the module proxy, which conflicts with "generated code is never committed". The public API of each hand-written layer becomes a compatibility promise once published.

## Upstream origin and license

Nothing is ported or copied. S0 runs upstream's MIT Python SDK from the pin as a test input. Upstream's `LICENSE` is MIT, copyright Mario Zechner, Can Bölük and Stencil Labs, Inc. (`upstream:LICENSE:1-5`); the Python SDK has its own MIT file (`upstream:sdk/python/omp-rpc/LICENSE:1-3`). `protox` is MIT or Apache-2.0 (`registry:protox-0.9.1/Cargo.toml:42`) and is already a workspace dependency. No NOTICE change is needed.

## Consequences

- Outside users get a versioned, language-neutral contract (S1) with per-file digests, and no new tool in the default build.
- Users and the owner get a measured answer on whether upstream's Python SDK works against `omp rpc` (S0), which is the part of the gap with a known use.
- No gRPC client is promised, so ADR 0048's WIRE premise and the existing daemon stay as they are.
- The gap stays partly open. Go and Python gRPC clients are deferred, with their preconditions recorded.

## Open questions for the owner

- **O1. ADR 0048 Decision 2.** Does publishing the S1 bundle without any SDK leave the revisit trigger unfired? Recommendation: yes.
- **O2. Scope of gap 18.** Is deferring generated gRPC clients until a named consumer exists acceptable? Recommendation: yes. The parity part of the gap is upstream's stdio SDKs, which S0 measures.
- **O3. Hello capabilities.** Should the daemon advertise `blob.v1` and the inference ids that `gateway.proto:15-18` lists, or should the contract text be corrected? This is an existing-server change, so this record does not decide it. Recommendation: decide it before any client record.
- **O4. `SessionControl` and Hello.** The session host's socket has no `Gateway` (`crates/driver/src/session_wire.rs:311-331`), although `session.proto:11-12` says negotiation uses it. Should `Gateway` be registered there? Recommendation: decide it with O3.
- **O5. Stdio SDK adoption.** Adopt upstream's MIT SDKs as dependencies for `omp rpc` if S0 passes for the 47 shared commands? Recommendation: decide on S0's table.
- **O6. Bundle publication.** Where is the bundle published: a workflow artifact per commit, or a release asset? In what container? Recommendation: a release asset built by `just proto-bundle` and packed by the release job.

## Acceptance tests

1. S1: the bundle holds 19 files, six services and 63 RPCs with their stream shapes. Its manifest's revisions equal `omp_proto::SCHEMA_REV` and `omp_rpc::MIN_SCHEMA_REV`, and its digests match the sources. Two builds are byte-identical. These tests run in CI's `just test`.
2. S1: a missing proto root and an uncompilable file each surface as a typed error.
3. S0: the probe classifies all 74 upstream commands, the 27 upstream-only names classify as `missing`, and P7 passes unchanged after the scripted daemon moves into support.

## What the review changed

### Round 1 (recorded in STATUS.md row 0069; the round's full text is not in this record)

Decision ids below are round-1 numbering. Round 2 renumbered them (see the end of the round-2 table).

- **BSR revisit trigger in ADR 0048 Decision 2.** Confirmed: `docs/adr/0048-schema-and-serialization-boundaries.md:65-67`. D2 quoted the trigger and dropped the "ADR 0048 amendment" slice. The revisit became O1.
- **`protox`, not `protoc`.** Confirmed: `AGENTS.md:656` (cited as `:654` before omp2 moved). D1 named `protox::compile`. A `protox`-hosted plugin runner replaced every `protoc` option.
- **S0 needs its own harness.** Confirmed: `upstream:sdk/python/omp-rpc/tests/test_client.py:1142-1149`, `upstream:sdk/go/omp-rpc/fake_test.go:21-26`, `upstream:sdk/go/omp-rpc/smoke_test.go:71-73`. The probe now drives upstream's SDK against the real `omp rpc` over P7's scripted daemon.
- **Command overlap recount, with the proto files cited.** Confirmed: the first count was wrong. The recount from `rpc-wire.schema.json:11` and `crates/app/src/rpc_mode.rs:1944-2911` gives 47 shared, 27 upstream-only and 11 omp2-side names. A per-service RPC table was added.
- **Server-older-than-client test missing.** Confirmed: `gateway.proto:55-56`, `crates/rpc/src/hello.rs:101-108`, `:190`. Handshake rules and scenarios were added. They are now deferred with the clients (Deferred item 6).

### Round 2 (EGM, four Goldfish reviewers plus synthesis: 2 blockers, 19 majors, 15 minors)

The resolution chosen: cut the record to S0 and S1, and defer client generation (M17, verified). With that cut, many findings no longer apply to this record. Those that state facts about the code are kept under "Deferred" for a later record.

| ID | Verified | Change or rejection |
|---|---|---|
| B1 `MIN_SCHEMA_REV` cycle | Yes: `crates/rpc/src/hello.rs:12`, `crates/rpc/Cargo.toml:16`; no `omp-rpc` dependency in `crates/proto/Cargo.toml` | D1 moves the bin to the standalone crate `crates/proto-bundle`, which depends on both crates and is linked into nothing, like `crates/roster-probe/Cargo.toml:1-20`. The manifest key `gateway_min_schema_rev` reads `omp_rpc::MIN_SCHEMA_REV` (`crates/rpc/src/lib.rs:31`) and cannot be confused with envd's 4 (`crates/envd/src/server/mod.rs:258`). |
| B2 scenarios contradict Hello capabilities | Yes: `gateway.proto:15-18`, `crates/app/src/daemon.rs:380-386`, `crates/rpc/src/hello.rs:157` | The scenarios are gone with the deferred clients. The conflict is Deferred item 1 and O3. Resolving it changes an existing Hello response, which is outside this additive record. |
| M1 32 MiB tests the server's limit | Yes: `crates/serve/src/blob.rs:13`; `registry:tonic-0.14.6/src/codec/mod.rs:101-102` | Scenario deferred; the fact is Deferred item 5. |
| M2 wrong status code | Yes: `registry:tonic-0.14.6/src/codec/decode.rs:192-195`, `crates/serve/src/blob.rs:142-147` | Test deferred; both codes are recorded in Deferred item 5. |
| M3 reference proof inconsistent | Yes: `crates/rpc/src/hello.rs:85-90`, `:109` | The proof is deferred; Deferred item 6. |
| M4 TCP and TLS helpers have no server | Yes: `crates/app/src/cli.rs:3923-3927`, `crates/app/src/auth_gateway_cmd.rs:33-40`; `server_tls` and `client_tls` are unused outside `crates/rpc` | Context now states the served transport correctly. The helpers are deferred; the `Auth` exposure is Deferred item 2. |
| M5 feature never compiled in CI | Yes: `justfile:72`, `:283` take no feature flags | The feature is removed. The new crate is a workspace member, so `just clippy` and `just test` cover it. There is no feature, so there is no unification hazard. |
| M6 `cap-*` misuse | Yes: `docs/adr/0049-tiered-integration-architecture.md:47`, `:94-101` | `cap-rpc-clients` is removed, and the ADR 0049 reference names the tiered-integration file. |
| M7 WIRE premise | Yes: `crates/proto/buf.yaml` breaking comment; ADR 0048 `:111-112` | No client is published; Deferred item 3. |
| M8 `SCHEMA_REV` as release identity | Yes: `crates/proto/src/lib.rs:45-51`; commits `24ea21e8db` (env-only rev bump) and `1f249d2fa9` (no bump) | Deferred item 4. S1's manifest adds per-file digests, so a consumer can see which files changed. |
| M9 distribution, Go module | Yes: ADR 0048 `:12` | Go is deferred (Deferred item 9). Bundle publication is O6. |
| M10 `scenarios.toml` unspecified | Yes: no schema in the round-2 text | Removed with the clients. |
| M11 S0 criteria, fixtures, ordering, launch | Yes: `crates/e2e/tests/p7_tui.rs:234-250`, `:1069-1087` | D3 defines pass, fail and missing. S0 adds `params.json`, one fresh process per command, scratch `HOME` and `OMP_DATA_DIR`, the full launch line, and a scripted reply for turn commands. `ScriptedGateway` moves into support (not copied), with P7 unchanged as an acceptance condition. |
| M12 Go before O2; CI cannot reach the pin | Yes: `git merge-base --is-ancestor c308970263 HEAD` is false | S0 is Python only, with a stated reason (one generated wire for all three SDKs), and there is no CI job. The person running it fetches the pin. Network use is stated. |
| M13 Go mapping covers 2 of 15 packages | Yes: no `go_package` in `crates/proto/proto` | Deferred item 8. |
| M14 public API left to the implementer | Yes (by inspection of the round-2 D3) | Deferred item 9. |
| M15 bundle format underspecified | Yes: `crates/proto/build.rs:26-32`; `Cargo.toml:61`, `:333` (no tar or zip crate) | D1 now gives the directory layout, the `buf.yaml` path, the manifest keys and their order, explicit input arguments (no `env!`, no git spawn), and the dirty flag. |
| M16 5 of 57 RPCs covered | Yes: `crates/app/src/daemon.rs:327-342` | Deferred item 7. |
| M17 no consumer; parity is S0 | Yes: `docs/research/rpc-clients.md` "Question" names none; the round-2 ADR said a daemon client "does not replace upstream's agent SDK" | **Accepted as the resolution:** the record is S0 and S1, and the clients are deferred (D4, O2). |
| M18 plugin host before its consumer | Yes: its only consumer was the Go client | Removed; see Rejected options. |
| M19 opt-in jobs go stale | Yes: `crates/app/tests/stock_sdk/` is referenced by no recipe or workflow | S1 runs in the default CI test job. S0 is stated to be a one-shot measurement with no CI run. Its compilation stays current; its run can go stale, and the record says so. |
| m1 "all Rust code behind the feature" | Yes | Moot: there is no feature. |
| m2 S2 dependency and seam | Yes | Moot: S2 is deferred. |
| m3 plugin host protocol details | Yes | Moot: the host is removed. |
| m4 missing `prost-types` runtime dependency | Yes: `crates/proto/Cargo.toml` (build dependency only) | Moot: the host is removed. The bundle crate's dependencies are named in D1. |
| m5 contradictory error rendering | Yes | Moot: the host is removed. D1's errors carry typed sources only. |
| m6 a second compile | Partly | **Rejected** for embedding from `OUT_DIR`, which would change `omp-proto`'s build and public surface (see Rejected options). The manifest's service list is derived from the same descriptor set in the same run, so it cannot drift. |
| m7 implementation detail in decisions | Yes | Moot for the removed host. D1 keeps only the format contract. |
| m8 O9's false binary choice | Plausible [INFERENCE] | Moot: Python clients are deferred. |
| m9 the Rust SDK not covered | Yes: `upstream:sdk/rust/omp-rpc` exists | D3 states why: one generated wire, and omp2 has its own Rust client. |
| m10 only 4 of 7 verbs documented as equivalent | Yes: `session.proto:172-188` | The Context text is corrected. |
| m11 session host not identified | Yes: `crates/app/src/chat_cmd.rs:1342-1345` | The Context table and text now name `omp chat`. |
| m12 ADR 0048:29 is not stale | Yes: it is Context that precedes its own Decision 4 (`docs/adr/0048-schema-and-serialization-boundaries.md:29`, `:81`) | The "stale" claim and the old O8 are withdrawn. |
| m13 `--data-dir` does not isolate `HOME` | Plausible: `crates/driver/src/registry.rs:246-254` | S0 sets a scratch `HOME` for every process. |
| m14 terms undefined; ledger row stale | Terms: yes | Terms are defined in the header. **The STATUS.md part is rejected here**: the driver brief bars editing `docs/adr/STATUS.md`, so the orchestrator updates row 0069. |
| m15 citation drift | Yes: `AGENTS.md:656`, `crates/app/src/rpc_mode.rs:2903-2911`, ADR 0048 `:12` | Citations are updated, and the 18-versus-19 file count is reconciled through commit `1f249d2fa9`. |

Renumbering: round 2 rewrote the decisions as follows.
- D1 is the bundle, now in its own crate.
- D2 is ADR 0048.
- D3 is the stdio probe (round-1 D8).
- D4 is the deferral. Round-1 D3-D7 (handshake layer, Go cycle, conformance, names and gates, plugin host) are folded into "Deferred".

This design has not been reviewed after this revision and is not owner-approved.

## Status in omp

**Status: Not started.** Reusable today:
- the proto sources and `buf.yaml`;
- `protox` (a workspace dependency);
- `omp_proto::SCHEMA_REV` and `omp_rpc::MIN_SCHEMA_REV`;
- the `crates/roster-probe` pattern for a crate linked into nothing;
- P7's scripted daemon.

Not yet implemented: `crates/proto-bundle`, `just proto-bundle`, `crates/e2e/src/support/scripted_gateway.rs`, `crates/e2e/tests/rpc_sdk_probe.rs`, and `scripts/rpc-sdk-probe/`.

## References

- [docs/research/rpc-clients.md](../research/rpc-clients.md); `docs/research/protocols-schemas-serialization.md`.
- Related ADRs:
  - ADR 0048, schema and serialization boundaries (Decisions 2, 3, 5).
  - ADR 0035, errors.
  - `docs/adr/0049-tiered-integration-architecture.md`, the `cap-*` tiers. No feature is added, and nothing here is a vendor integration.
  - ADRs 0006, 0024 and 0036 were checked: there is no sandbox, roster or extension-runtime change.
- Code: `crates/proto`, `crates/rpc/src/hello.rs`, `crates/rpc/src/framing.rs`, `crates/app/src/daemon.rs`, `crates/app/src/rpc_mode.rs`, `crates/driver/src/session_wire.rs`, `crates/e2e/tests/p7_tui.rs`, `crates/roster-probe`.
