# OMP2 IPC and serialization audit

Back to the [index](../README.md). Audited commit `85d219ae7f`; worktree `/private/tmp/omp-gateway-research` (no `crates/` differences from the audited commit).

## 1. Method

Read `crates/proto/**`, `crates/serve`, `crates/journal`, the envd framing code, the extension-host CONTROL code, `crates/memory/src/embedding`, ADR 0048, ADR 0069 (context only) and `docs/research/protocols-schemas-serialization.md`. Counts came from scripts over the proto files, not from the research file. Searches for absent items: exact-name grep for `CodeIR`, `ChangeIR`, `VerificationIR`, `CodeQueryIR` (and `Ir` variants) in `crates`, `extensions`, `packages`, `docs/adr`, `docs/architecture`, `docs/design`; package-name search in `Cargo.lock` for `capnp`, `flatbuffers`, `rmp`, `rmp-serde`, `rkyv`; grep for `shm_open`, `shared_memory`, `memfd_create`, `ipc-channel`, `shmem`. Nothing was measured.

## 2. Schema inventory (counted by script)

| Package file | Lines | Messages | Enums | Oneofs | RPCs |
|---|---|---|---|---|---|
| `auth/v1/auth.proto` | 500 | 47 | 6 | 2 | 21 |
| `blob/v1/blob.proto` | 84 | 7 | 0 | 0 | 4 |
| `collab/v1/collab.proto` | 313 | 40 | 8 | 3 | 0 |
| `control/v1/control.proto` | 371 | 35 | 6 | 3 | 0 |
| `document/v1/document.proto` | 1286 | 106 | 26 | 12 | 0 |
| `env/v1/env.proto` | 2279 | 214 | 32 | 26 | 0 |
| `gateway/v1/gateway.proto` | 63 | 3 | 0 | 0 | 2 |
| `identity/v1/identity.proto` | 45 | 8 | 0 | 0 | 0 |
| `inference/v1/common.proto` | 132 | 9 | 4 | 1 | 0 |
| `inference/v1/inference.proto` | 1085 | 87 | 24 | 11 | 30 |
| `inference/v1/media.proto` | 311 | 20 | 8 | 2 | 0 |
| `inference/v1/models.proto` | 204 | 12 | 6 | 1 | 0 |
| `inference/v1/search.proto` | 132 | 6 | 3 | 0 | 0 |
| `policy/v1/policy.proto` | 198 | 15 | 1 | 0 | 0 |
| `session/v1/session.proto` | 311 | 25 | 0 | 3 | 6 |
| `telemetry/v1/telemetry.proto` | 288 | 21 | 0 | 1 | 0 |
| `thread/v1/thread.proto` | 178 | 11 | 4 | 2 | 0 |
| `toolhost/v1/toolhost.proto` | 1487 | 163 | 14 | 24 | 0 |
| `ui/v1/ui.proto` | 390 | 56 | 1 | 8 | 0 |
| Total | 9657 | 885 | 143 | 99 | 63 |

Streaming RPCs: `Auth.WatchCredentials` (server), `Blob.Get` (server), `Blob.Put` (client), `Inference.Turn` and `Realtime` (bidirectional), `GenerateImage`, `Speak`, `AttachGeneration`, `Native`, `WatchModels` (server), `SessionControl.Attach` (server). `toolhost`, `env`, `document`, `collab` have no service; they are frame envelopes over custom transports.

## 3. Verification of `docs/research/protocols-schemas-serialization.md`

| Claim in research file | Finding at audited commit |
|---|---|
| 18 files, 14 packages | 19 files, 15 packages: `session/v1/session.proto` was added (`crates/proto/proto/omp/session/v1/session.proto:16` `SessionControl`) |
| 855 messages, 141 enums, 109 oneofs | 885, 143, 99 (the research file's per-file oneof rows sum to 96 while its total says 109; session adds 3 to the rows) |
| 5 services, 57 RPCs | 6 services, 63 RPCs |
| 9,258 lines | 9,657 lines |
| `SCHEMA_REV` 27 (also in ADR 0048) | 30 (`crates/proto/src/lib.rs:51` `SCHEMA_REV`); `crates/py/python/omp/limits.py:39` `SCHEMA_REV` also 30 |
| envd Hello accepts 4 to 27 | lower bound 4 still true (`crates/envd/src/server/mod.rs:258` `MIN_SCHEMA_REV`); upper bound is the current constant; check moved to `crates/envd/src/server/session.rs:1976` |
| gateway accepts from 1 | true (`crates/rpc/src/hello.rs:12` `MIN_SCHEMA_REV`) |
| `buf breaking` adopted with `WIRE`, ignoring control, telemetry, ui, collab | true (`crates/proto/buf.yaml:13` `WIRE`) |
| Python host speaks `omp.toolhost.v1` | not for the extension host: CONTROL is length-prefixed JSON (`crates/envd/src/exthost/control.rs:3776` `read_json_control_frame`); Protobuf `WorkerFrame` is used by the same-binary named worker |
| Embedding worker not covered | NDJSON worker found and bounded (section 4) |

Not verified: the technology comparison tables (sections 3 and 4 of the research file), the Bazel and BuildBuddy sections, REAPI inventory and benchmark plan 14.1 beyond noting that none was run.

## 4. Embedding worker protocol

`InboundFrame` (`Ping`, `Init`, `Embed`) and `OutboundFrame` (`Pong`, `Ready`, `Vectors`, `Error`, `Log`), tagged by `type`, `deny_unknown_fields` (`crates/memory/src/embedding/protocol.rs:29` `InboundFrame`). Bounds: 8 MiB frame, 256 texts, 4 MiB aggregate text, 4096 dimensions, 32 rows per outbound frame. Validation re-serializes the frame to measure it (`protocol.rs:92`, `protocol.rs:191`). The supervisor reads with a size cap and a generation fence and kills on drop (`crates/memory/src/embedding/supervisor.rs:260` `fill_buf`). Computed: 32x384 f32 = 49,152 raw bytes; a 1000x384 batch = 1,536,000 raw bytes.

## 5. Other findings

- `crates/serve/src/blob.rs:125` `bytes` buffers a whole upload (cap 64 MiB) and hashes afterwards, while `blob.proto` says the server hashes as bytes arrive. `crates/journal/src/blob.rs:561` `put_reader` exists.
- envd frame readers allocate the declared length after the limit check (`crates/envd/src/server/forward.rs:2992` `read_server_frame`).
- tonic default decode limit is overridden to 64 MiB on the daemon (`crates/app/src/daemon.rs:70` `max_decoding_message_size`); session control uses 16 MiB.
- Journal: SSE frames capped at 1 MiB payload plus 64 KiB; unknown kinds fail closed (`crates/journal/src/kind.rs:131` `is_known`). ADR 0048 status: Partially implemented (B9 open).
- ADR 0069: Proposed, deferral only; it fixes that `omp rpc` is NDJSON and the daemon serves only local endpoints; no code consequence for serialization.
- CodeIR, ChangeIR, VerificationIR, CodeQueryIR: Absent. Existing typed IR: `crates/shell/src/analysis.rs:45` `ScriptIr`, `crates/shell/src/analysis.rs:104` `CommandIr`. kb stores serde_json and decodes SCIP with the `protobuf` crate (`crates/kb/src/graph.rs:9` `Message`).

Back to the [index](../README.md).
