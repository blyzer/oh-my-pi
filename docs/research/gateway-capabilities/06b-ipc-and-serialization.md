# IPC and serialization: payload classes, capability cards and benchmark plan (Part B of deliverable 6)

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 6. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Evidence: [omp2-ipc-serialization.md](evidence/omp2-ipc-serialization.md), [omp2-ipc-serialization.tsv](evidence/omp2-ipc-serialization.tsv), [ref-serialization.md](evidence/ref-serialization.md). Part A (transport) is in `06-transport-and-protocol-recommendations.md` once the parent assembles it.

## 1. Summary

- OMP2 already uses five formats, each at one boundary: Protobuf (process and network), SSE plus JSON (the `.oms` journal), postcard (private local stores), JSON-RPC or NDJSON (external standards, `omp rpc`, embedding worker) and length-prefixed JSON (extension-host CONTROL). ADR 0048 decided to keep them.
- **Nothing was measured in this pass.** Every performance statement is either arithmetic on code constants (marked computed) or `not measured` with a plan in section 6. The out-of-tree benchmark in `/private/tmp/omp-research-bench-ipc/` was not built.
- `CodeIR`, `ChangeIR`, `VerificationIR` and `CodeQueryIR` do not exist as types, schemas or ADR text. The large-IR question is therefore a plan, not a finding.
- Findings that change what the parent should say: the research file `docs/research/protocols-schemas-serialization.md` is stale (18 files, 14 packages, `SCHEMA_REV` 27; now 19, 15 and 30); the extension-host CONTROL channel is JSON, not Protobuf (IPC-06); the blob upload buffers whole blobs although its contract says it hashes as bytes arrive (IPC-03).
- Reference reading shows every zero-copy candidate (Cap'n Proto, FlatBuffers, rkyv) needs a verifier on untrusted input and a codegen or derive step, and that rkyv has no schema evolution story comparable to Protobuf tags. That supports ADR 0048's caution but not its claim of having evidence; see IPC-05.

| Card | Title | Decision class |
|---|---|---|
| IPC-01 | Keep Protobuf as the contract | Already implemented: reuse |
| IPC-02 | Compact vector framing for embedding worker | Requires benchmarking |
| IPC-03 | Blob upload hashes while streaming | Partially implemented: extend |
| IPC-04 | Large IR as Protobuf plus CAS reference | Requires benchmarking |
| IPC-05 | Reopen unmeasured zero-copy rejection | Requires ADR reconsideration |
| IPC-06 | Document CONTROL as JSON | Partially implemented: extend |
| IPC-07 | MessagePack for internal channels | Unnecessary or redundant |
| IPC-08 | Shared memory and mmap for local bulk | Unnecessary or redundant |
| IPC-09 | Allocation-bounded frame reads, optional compression | Suitable as an optional extension |

## 2. Wire inventory (verified against code)

| Boundary | Format and framing | Bound | Evidence |
|---|---|---|---|
| App or peer to envd | Protobuf `ClientFrame` and `ServerFrame`, varint length-delimited, over UDS, stdio or in-process | 64 MiB | `crates/envd/src/server/forward.rs:2992` `read_server_frame`, `crates/envd/src/server/mod.rs:259` `FRAME_LIMIT`, `crates/proto/proto/omp/env/v1/env.proto:12` |
| envd to document authority | Protobuf, varint length-delimited | 64 MiB | `crates/envd/src/docserver/wire.rs:14` `DEFAULT_MAX_FRAME_BYTES` |
| envd to extension host (CONTROL) | JSON, 4-byte big-endian length, Unix socketpair on descriptor 3 | 64 MiB, 1 MiB per dispatch progress frame | `crates/envd/src/exthost/control.rs:3776` `read_json_control_frame`, `crates/envd/src/exthost/control.rs:1045` `MAX_CONTROL_FRAME_BYTES`, `crates/py/python/omp/_host.py:224` `struct` |
| envd to same-binary named worker | Protobuf `HostFrame` and `WorkerFrame` on piped stdio | `FRAME_MAX_BYTES` | `crates/envd/src/worker_child.rs:132` `FRAME_MAX_BYTES`, `crates/envd/src/exec.rs:4000` `take_worker_frame` |
| Daemon gRPC (inference, auth, blob, gateway, session control) | Protobuf over tonic, HTTP/2 | 64 MiB per message (16 MiB for session control) | `crates/app/src/daemon.rs:63` `RPC_MESSAGE_MAX_BYTES`, `crates/driver/src/session_wire.rs:82` `CONTROL_MESSAGE_MAX_BYTES` |
| Memory embedding worker | NDJSON over piped stdio, generation-fenced | 8 MiB per frame, 256 texts, 4096 dimensions | `crates/memory/src/embedding/protocol.rs:12` `MAX_FRAME_BYTES`, `crates/memory/src/embedding/supervisor.rs:247` `to_vec` |
| Journal `.oms` | SSE frames with single-line JSON | 1 MiB payload plus 64 KiB overhead | `crates/journal/src/sse.rs:19` `DATA_HARD_CAP`, `crates/journal/src/sse.rs:33` `FRAME_HARD_CAP` |
| `omp rpc` | NDJSON with protocol-v2 chunk reassembly | 1 MiB line, 64 MiB reassembled (per ADR 0069) | `crates/rpc/src/framing.rs:1` |
| ACP, MCP, LSP, DAP | JSON-RPC (newline or Content-Length framed) | per protocol | `crates/app/src/acp_mode.rs:1`, `crates/envd/src/mcp/transport/stdio.rs:1` |

Protobuf code generation: `crates/proto/build.rs:55` `.bytes(".")` makes `bytes` fields `Bytes`, `crates/proto/build.rs:56` `btree_map` makes maps deterministic, and every type derives serde (Rust-native, not the proto3 JSON mapping). The schemas total 19 files, 15 packages, 9,657 lines, 885 messages, 143 enums, 99 oneofs, 6 services and 63 RPCs (counted by script over `crates/proto/proto/omp/**`, nested messages included). Exactly one field in the whole schema set is a repeated float: the embedding vector (`crates/proto/proto/omp/inference/v1/inference.proto:895` `values`).

Compatibility: `SCHEMA_REV` is 30 (`crates/proto/src/lib.rs:51` `SCHEMA_REV`), the gateway accepts from 1 (`crates/rpc/src/hello.rs:12` `MIN_SCHEMA_REV`), envd accepts 4 to 30 (`crates/envd/src/server/mod.rs:258` `MIN_SCHEMA_REV`). `buf breaking` with `WIRE` is configured (`crates/proto/buf.yaml:13` `WIRE`), ignoring `control`, `telemetry`, `ui`, `collab`.

## 3. Per-payload-class analysis

### 3.1 Control messages (small, frequent)

Hello, request or response frames, approvals, cancels. Today: Protobuf frames on envd and gRPC, JSON on CONTROL. Size is tens to thousands of bytes (computed from field lists, not sampled). Serialization cost is dominated by process scheduling and syscalls, not by the codec, so a faster codec cannot be justified without a profile. Complexity is the deciding factor: Protobuf already carries tag-based evolution, and `buf breaking` guards it. Verdict: IPC-01 reuse; IPC-06 documentation fix; IPC-07 no change.

### 3.2 Token streams (small, very frequent)

`TurnEvent.part_delta` carries `PartDelta { index, bytes chunk }` (`crates/proto/proto/omp/inference/v1/inference.proto:683` `chunk`) over a bidirectional `Turn` RPC (`crates/proto/proto/omp/inference/v1/inference.proto:48` `Turn`). The chunk is UTF-8 text, thinking text or a tool-argument JSON fragment. Per-event overhead is a gRPC five-byte frame header plus Protobuf tags; JSON fields inside tool-argument fragments are not parsed on this path. Real cost is the number of events per second and downstream work, not the codec. The `.oms` journal stores the same content as JSON by design. Not measured. Verdict: no format change; plan case T1.

### 3.3 Embedding batches (medium, dense floats)

Two paths. gRPC `Embed` returns packed `repeated float` per vector. The memory worker pipe returns JSON arrays, 32 rows per frame, and re-serializes each frame to check its size (IPC-02). Computed sizes: a 1000x384 f32 batch is 1,536,000 raw bytes (1.46 MiB). The worker protocol allows 256 texts per request (`crates/memory/src/embedding/protocol.rs:14` `MAX_TEXTS`), so a 1000-row batch is at least four requests today. One 32x384 frame is 49,152 raw bytes. JSON text size, parse time and allocation count are `not measured`. Verdict: IPC-02.

### 3.4 Large IR (CodeIR, ChangeIR, VerificationIR)

These types are absent (searched by exact name). The nearest existing artifacts are `ScriptIr` and `CommandIr` (shell effects, projected through `policy.proto`), the kb index (serde_json) and SCIP indexes (Protobuf, decoded with the `protobuf` crate, a second Protobuf runtime in the workspace). Sparse-read access patterns would favour an indexable format, but no access pattern exists yet. Verdict: IPC-04 (measure size distribution first), IPC-05.

### 3.5 Artifact references

Content-addressed, SHA-256, rendered as `artifact://sha256/<hex>`. `Blob` has `Stat`, ranged `Get` stream, `Put` stream and `Delete` (`crates/proto/proto/omp/blob/v1/blob.proto:13` `Blob`). Chunk size is 64 KiB on download (`crates/serve/src/blob.rs:13` `CHUNK_SIZE`). The store streams ranges (`crates/journal/src/blob.rs:637` `open_range`) but `get` reads whole files (`crates/journal/src/blob.rs:616` `get`). Upload buffering is IPC-03. This is the existing answer for any payload too large to inline and should be the default for large IR.

### 3.5a Concurrent swarm workloads

The owner brief lists concurrent swarm workloads. No code path here is swarm-specific; multi-agent fan-out reuses the same frames. Concurrency risks found: per-frame scratch buffers sized by declared length (IPC-09) and per-upload buffering (IPC-03). Contention was not measured.

## 4. Codegen and schema-evolution constraints (from reference reading)

See [ref-serialization.md](evidence/ref-serialization.md). Short form:

| Format | Evolution | Untrusted-input guard | Codegen | Streaming |
|---|---|---|---|---|
| Protobuf (prost) | tags; unknown fields skipped on decode | decode limits, recursion limit | build.rs, protox (no protoc) | length-delimited framing by caller; tonic streams messages |
| Cap'n Proto | pointer-based schema evolution (docs, not code-read) | `ReaderOptions` traversal and nesting limits | `capnpc` build step | `capnp-futures` read and write messages |
| FlatBuffers | table fields append-only (docs, not code-read) | `Verifier`, `root` versus `root_unchecked` | `flatc` external binary | no stream framing in the crate |
| rkyv | none comparable; layout follows Rust types | optional `bytecheck` validation | derive macros, no IDL | none; needs aligned in-memory buffer |
| MessagePack, JSON | self-describing | parser limits by caller | serde derives | NDJSON by caller |

## 5. Capability cards

### IPC-01 Keep Protobuf (prost/tonic) as the process and network contract
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** crates/proto/build.rs:64 `compile_fds` compiles 19 `.proto` files in 15 packages with protox and tonic-prost-build; `crates/serve/src/lib.rs:1` serves them over tonic; `crates/proto/buf.yaml:13` `WIRE` breaking check; ADR 0048 decision 1 (Implemented for the format, status Partially implemented overall).
**Actual missing capability:** None for the format. Remaining gaps are listed in IPC-06 and IPC-09.
**Alternatives considered:** Replace with Cap'n Proto, FlatBuffers or rkyv (see IPC-05); add MessagePack (see IPC-07). No measurement supports a change.
**Expected benefit:** Avoids migration cost and keeps one schema authority. Benefit is risk avoidance, not speed.
**Implementation complexity:** low (no change)
**Security implications:** Unchanged. Frame ceilings exist at every Protobuf boundary (`crates/proto/src/bounds.rs:11` `FRAME_MAX_BYTES`, `crates/app/src/daemon.rs:63` `RPC_MESSAGE_MAX_BYTES`).
**Maintenance cost:** Low; already paid.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** Every new cross-process payload class is first expressed as a message in `crates/proto/proto/omp/**` unless an ADR says otherwise; `buf breaking` stays in the `protobuf` CI job.
**Follow-up:** Cap'n Proto, FlatBuffers and rkyv are not a prerequisite for any item below.

### IPC-02 Embedding batches: compact vector framing for the memory embedding worker
**Decision class:** Requires benchmarking
**Existing implementation evidence:** crates/memory/src/embedding/protocol.rs:141 `OutboundFrame` carries `vectors: Vec<Vec<f32>>` as newline-delimited JSON, 32 rows per frame (`protocol.rs:20` `VECTOR_FRAME_ROWS`), 8 MiB frame cap (`protocol.rs:12` `MAX_FRAME_BYTES`); Prototype-grade isolation, `Implemented` on the production path. The gRPC `Embed` RPC already carries packed `repeated float` (`crates/proto/proto/omp/inference/v1/inference.proto:893` `EmbedResponse`).
**Actual missing capability:** No measurement of JSON float text cost for 384-dimension rows. `validate` re-serializes every frame to check its size (`protocol.rs:92`, `protocol.rs:191` `to_vec`), so each frame is serialized twice in each direction: once to send and once for the size check (on the sender when the parent writes a request, and on the parent when it validates a worker reply, `supervisor.rs:113`, `:172`, `:247`, `:278-279`). Raw payload for 1000x384 f32 is 1,536,000 bytes (computed); JSON size and CPU are not measured.
**Alternatives considered:** (a) keep NDJSON; (b) length-prefixed raw little-endian f32 rows after a small JSON header; (c) Protobuf `EmbedResponse` frames reusing `omp_proto`; (d) MessagePack with `rmp-serde` (adds a dependency, no schema); (e) shared memory (rejected as premature, see IPC-08).
**Expected benefit:** Possible latency and CPU reduction on the parent/worker pipe, and memory (no per-float text parse). Magnitude not measured.
**Implementation complexity:** medium: touches parent, worker, tests and the generation fence; protocol is `deny_unknown_fields` so a version bump needs a deliberate path.
**Security implications:** A binary frame needs the same bound checks as today (`MAX_VECTOR_DIMENSIONS`, row count, finiteness). Raw f32 must still reject NaN and infinity.
**Maintenance cost:** Medium if a second framing is added; low if (c) reuses `omp_proto` types.
**Performance measurement strategy:** Benchmark plan section 6, cases E1 to E5, 1000x384 f32 batch; compare serde_json, prost and rmp-serde, then the NDJSON `OutboundFrame` path. `not measured`.
**Acceptance criteria:** A recorded benchmark with command, environment and raw output; change adopted only if round-trip time or allocation drops by a margin fixed before the run; existing worker tests keep passing; a corrupted or oversized frame is rejected.
**Follow-up:** extend `crates/memory` if the benchmark passes (owner of L01 is the inference area).

### IPC-03 Blob upload that hashes while streaming instead of buffering the whole blob
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** crates/serve/src/blob.rs:120 `put` collects the stream into a `Vec` (`blob.rs:125` `bytes`), checks the 64 MiB cap (`blob.rs:14` `MAX_UPLOAD_BYTES`) and hashes afterwards (`blob.rs:152` `validate_upload`). `crates/journal/src/blob.rs:561` `put_reader` exists and `open_range` (`journal/src/blob.rs:637`) already serves ranged reads. `blob.proto:13` `Blob` states the server hashes bytes as they arrive.
**Actual missing capability:** The service documentation promises incremental hashing; the implementation buffers up to 64 MiB per concurrent upload, which matters for concurrent swarm uploads. `Blob.Get` already streams 64 KiB chunks (`blob.rs:13` `CHUNK_SIZE`).
**Alternatives considered:** Leave as is (bounded at 64 MiB, owner-local peer); lower the cap; stream to a staging file through `put_reader`.
**Expected benefit:** Memory (peak per upload drops from blob size to chunk size) and an honest contract.
**Implementation complexity:** low to medium: bridge an async stream to the blocking `put_reader`, keep size and digest checks.
**Security implications:** Must keep first-chunk-only hash and size rules and the 64 MiB cap; staging files need the existing fsync and rename discipline in `crates/journal/src/blob.rs`.
**Maintenance cost:** Low.
**Performance measurement strategy:** Peak RSS during N concurrent 64 MiB uploads, before and after. `not measured`.
**Acceptance criteria:** An upload of 64 MiB holds no more than a bounded number of chunks in memory; digest and size mismatches still fail with the same status codes; tests cover interrupted and idempotent re-puts.
**Follow-up:** update the `blob.proto` comment instead if the owner prefers buffering.

### IPC-04 Large IR payloads (CodeIR, ChangeIR, VerificationIR): Protobuf message plus content-addressed reference
**Decision class:** Requires benchmarking
**Existing implementation evidence:** None of `CodeIR`, `ChangeIR`, `VerificationIR` or `CodeQueryIR` exists in `crates`, `extensions`, `packages`, `docs/adr`, `docs/architecture` or `docs/design` (searched by exact name, case-sensitive and `Ir` variants; only the owner brief names them). Existing typed IR: `crates/shell/src/analysis.rs:45` `ScriptIr` and `crates/shell/src/analysis.rs:104` `CommandIr`, projected as shell-effect messages in `policy.proto`; the knowledge-base index is serde_json (`crates/kb/src/store.rs:281` `to_writer`) and SCIP is decoded with the `protobuf` crate (`crates/kb/src/graph.rs:9` `Message`), a second Protobuf runtime next to prost. Artifact references exist: `artifact://sha256/<hex>` over `Blob` and `BlobRef`.
**Actual missing capability:** The IR types do not exist, so there is nothing to size. No proto, no size distribution, no decision on inline versus reference.
**Alternatives considered:** Inline Protobuf messages under the 64 MiB frame cap; Protobuf plus a `BlobRef` for payloads above a threshold; FlatBuffers or Cap'n Proto for random access without a full decode; rkyv inside one Rust process.
**Expected benefit:** If the IR is large and read sparsely, partial decode or a reference could cut copying and memory. Not demonstrated.
**Implementation complexity:** medium (schema design) once a consumer exists
**Security implications:** Zero-copy readers touch untrusted bytes in place; every candidate needs a verifier (see ref-serialization evidence). Protobuf plus CAS keeps the existing hash check.
**Maintenance cost:** Low if Protobuf; higher for any second IDL (second codegen, second compatibility regime).
**Performance measurement strategy:** Benchmark plan section 6, cases L1 to L4 on a synthetic IR shaped from a real SCIP index of this repository (size distribution first). `not measured`.
**Acceptance criteria:** IR size distribution recorded first; a threshold for inline versus reference chosen from it; the chosen encoding round-trips under `buf breaking`; frame and field bounds tested.
**Follow-up:** do not create proto messages until a consumer is named (ADR 0069 applies the same rule to clients).

### IPC-05 Zero-copy formats (Cap'n Proto, FlatBuffers, rkyv): reconsider the unmeasured rejection
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** docs/adr/0048-schema-and-serialization-boundaries.md alternatives section: zero-copy formats were "rejected without measurement"; the same ADR notes rkyv cannot evolve schemas and FlatBuffers has an advisory history. Workspace: no `capnp`, `flatbuffers` or `rmp` package in `Cargo.lock`; `rkyv` 0.8.18 appears only transitively through `dtls` (searched `Cargo.lock`).
**Actual missing capability:** The owner brief asks for a benchmark and forbids adopting zero-copy on theory alone. ADR 0048 contains a rejection without data. Reference reading (ref-serialization evidence) shows each candidate needs a verifier and has different evolution rules, so the rejection is plausible but not evidenced locally.
**Alternatives considered:** Keep the rejection as written; reopen only if IPC-02 or IPC-04 measures a bottleneck that Protobuf cannot remove.
**Expected benefit:** Prevents both premature adoption and an unfalsifiable rejection.
**Implementation complexity:** low (ADR text only)
**Security implications:** Zero-copy access to untrusted bytes without a verifier is memory-unsafe or panics; FlatBuffers and rkyv expose unchecked entry points.
**Maintenance cost:** Low.
**Performance measurement strategy:** Inputs come from IPC-02 and IPC-04 benchmarks.
**Acceptance criteria:** ADR 0048 alternatives text is amended to cite the benchmark result or to say the question stays closed until a named workload exceeds a stated budget.
**Follow-up:** Class 6 follow-up: reopen only when a benchmark fires.

### IPC-06 Extension-host CONTROL channel: document that it is length-prefixed JSON, not Protobuf
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** CONTROL is a Unix socketpair carrying 4-byte big-endian length then JSON (`crates/envd/src/exthost/control.rs:3776` `read_json_control_frame`, `crates/envd/src/exthost/control.rs:2549` `JsonControlFrame`, 64 MiB cap `crates/envd/src/exthost/control.rs:1045` `MAX_CONTROL_FRAME_BYTES`, `crates/py/python/omp/_host.py:224` `struct`, `crates/envd/src/exthost/spawn.rs:545` `UnixStream`). `crates/proto/proto/omp/toolhost/v1/toolhost.proto:15` still calls it the CONTROL protocol with varint-length-delimited frames; the Protobuf `HostFrame` and `WorkerFrame` are used by the same-binary named worker (`crates/envd/src/worker_child.rs:132` `FRAME_MAX_BYTES`, `crates/envd/src/exec.rs:4000` `take_worker_frame`). `docs/architecture/processes.md` describes CONTROL as JSON.
**Actual missing capability:** The `.proto` header and ADR 0048 wording imply Protobuf for the Python extension host; the code path is JSON. Compatibility there is not covered by `buf breaking` and is enforced only by Python and Rust code staying in step.
**Alternatives considered:** Move CONTROL to Protobuf (large change, Python side needs a decoder in a stdlib-only host); keep JSON and document it; add MessagePack.
**Expected benefit:** Reliability (correct expectations for reviewers and schema checks).
**Implementation complexity:** low (comment and doc correction); high if CONTROL itself were migrated
**Security implications:** JSON with a 64 MiB cap is parsed in the Rust parent from a sandboxed child; that is the existing trust boundary.
**Maintenance cost:** Low.
**Performance measurement strategy:** not applicable for the documentation fix; any migration gets its own benchmark.
**Acceptance criteria:** `toolhost.proto` header and ADR 0048 context state which toolhost frames are Protobuf and which channel is JSON; a test or doc check ties `SCHEMA_REV` in `crates/py/python/omp/limits.py:39` to the Rust constant.
**Follow-up:** no migration proposed.

### IPC-07 MessagePack for internal JSON channels
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** JSON channels today: embedding NDJSON, extension CONTROL, journal SSE+JSON, `omp rpc` NDJSON (`crates/rpc/src/framing.rs:1`). No `rmp` or `rmp-serde` in `Cargo.lock`.
**Actual missing capability:** No measured problem in any of these channels. Journal and `omp rpc` must stay text (ADR 0048 decision 1); CONTROL has a Python peer; the embedding channel is covered by IPC-02 where MessagePack is one benchmark arm.
**Alternatives considered:** Adopt for CONTROL and embedding; adopt nowhere.
**Expected benefit:** None demonstrated.
**Implementation complexity:** low
**Security implications:** A new parser on untrusted input and a new dependency.
**Maintenance cost:** Low to medium.
**Performance measurement strategy:** MessagePack arm of the IPC-02 benchmark; `not measured`.
**Acceptance criteria:** Reopen only if the IPC-02 benchmark shows MessagePack beats the best Protobuf or raw-f32 framing by a margin fixed beforehand.
**Follow-up:** Classification is a judgement from architecture, not a measurement.

### IPC-08 Shared memory and memory-mapped files for local bulk transfer
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** No shared-memory or `memfd` use anywhere in `crates`, `extensions` or `packages` (searched `shm_open`, `shared_memory`, `memfd_create`, `ipc-channel`, `shmem`). `memmap2` is used only inside `crates/shell-builtins` (`src/tac.rs:15` `Mmap`, `src/jq.rs:644`, `src/sed.rs:5305`), always for local file reads, never for IPC. Large payloads already cross processes as content-addressed files via `Blob`, with ranged reads.
**Actual missing capability:** No workload crosses a process boundary with a payload that exceeds the 64 MiB frame ceiling or that is copy-bound; swarm workloads are described in the swarm area, not here.
**Alternatives considered:** memfd or shm segments for embedding batches; mmap of CAS blobs by readers on the same host.
**Expected benefit:** Would save copies only above the size where Unix-socket copies dominate; not measured.
**Implementation complexity:** high: lifetime, fencing, cleanup, portability to Windows and macOS sandboxes.
**Security implications:** Shared segments cross sandbox boundaries and weaken isolation; mmap of a file that another process truncates can fault.
**Maintenance cost:** High.
**Performance measurement strategy:** Include a socket-copy versus mmap arm for CAS blob reads in case L4 only if IPC-04 shows a large-read bottleneck; `not measured`.
**Acceptance criteria:** Reopen when a measured workload spends a stated share of wall time in IPC copies.
**Follow-up:** Judgement from architecture, not a measurement.

### IPC-09 Decode-time allocation and optional compression on Protobuf boundaries
**Decision class:** Suitable as an optional extension
**Existing implementation evidence:** envd frame readers check the length against the 64 MiB limit and then `resize` a scratch buffer to the full declared length before reading (`crates/envd/src/server/forward.rs:2992` `read_server_frame`), so a peer that declares a large length makes the reader allocate and zero that much. tonic servers set 64 MiB decode and encode limits explicitly (`crates/app/src/daemon.rs:70` `max_decoding_message_size`); the session control service uses 16 MiB (`crates/driver/src/session_wire.rs:82` `CONTROL_MESSAGE_MAX_BYTES`). The workspace enables tonic with `tls-ring` only (`Cargo.toml:255`), so gzip and zstd compression are not compiled in.
**Actual missing capability:** Incremental read (grow the buffer as bytes arrive) on envd frames; optional tonic compression for the bearer-authenticated TCP daemon. Both are unmeasured.
**Alternatives considered:** Leave as is (owner-local UDS peers); lower the limit; enable compression globally (rejected: local UDS gains nothing).
**Expected benefit:** Memory (bounded allocation under a hostile or buggy peer); reduced network transmission on TCP only.
**Implementation complexity:** low (frame reader), low (feature flag)
**Security implications:** The frame reader change is a hardening item; compression on TLS carries a known side-channel class for secrets in compressed payloads, so it must stay off for credential-bearing RPCs.
**Maintenance cost:** Low.
**Performance measurement strategy:** Peak RSS with a declared-length-only frame; TCP bytes on the wire for a large `Turn` with and without compression. `not measured`.
**Acceptance criteria:** A frame that declares 64 MiB but sends 1 KiB allocates about 1 KiB; compression, if added, is off for the `Auth` service.
**Follow-up:** extension only; not part of the default path.

## 6. Benchmark plan (`not measured`)

Nothing below has been run. Run it out of tree in `/private/tmp/omp-research-bench-ipc/`, never inside the repository, and save raw output to `evidence/measurements/NN-ipc-<slug>.md` with command, `uname -m`, memory, core count, `uptime` and run count (rule 5).

| Case | Payload | Arms | Metrics |
|---|---|---|---|
| E1 | 1000x384 f32 (random, finite) as one batch | serde_json `Vec<Vec<f32>>`; prost `EmbedResponse`; rmp-serde `Vec<Vec<f32>>`; raw little-endian f32 plus header | encoded bytes, encode and decode time, allocations, peak RSS |
| E2 | 32x384 f32, the worker's current frame | same arms | same, and per-frame fixed cost |
| E3 | E1 through a real pipe to a child process | NDJSON versus length-prefixed binary | end-to-end latency, throughput |
| E4 | E1 with the worker's size check | encode once versus encode-then-validate | CPU |
| E5 | corrupted and oversized frames | all arms | rejection correctness |
| T1 | 10,000 `PartDelta` events of 4 to 40 bytes | gRPC stream over UDS; envd varint frames | events per second, CPU per event |
| L1 | synthetic IR sized from a real SCIP index of this repository | Protobuf inline; Protobuf plus CAS reference | size, decode time, peak RSS |
| L2 | same | FlatBuffers with verifier (arm only if L1 shows decode dominating) | sparse-read time |
| L3 | same | Cap'n Proto and rkyv with validation (same condition) | sparse-read time, validation cost |
| L4 | 64 MiB CAS blob read | socket stream versus mmap | copies, RSS, time |
| C1 | 8 and 32 concurrent uploads of 64 MiB | current `put` versus streaming `put_reader` | peak RSS |

Controls: warm-up runs, at least 30 timed runs, report median and spread, pin the build profile (`--release`), record load average because other work shares the machine. Decision rule fixed before running: adopt a non-Protobuf arm only if it wins by a pre-stated margin on end-to-end latency or memory, not on codec time alone.

## 7. Draft ADR needed?

| Card | Draft ADR needed? | Reason |
|---|---|---|
| IPC-01 | No | ADR 0048 covers it |
| IPC-02 | Only if the benchmark passes | it would amend the memory worker protocol |
| IPC-03 | No | implementation fix; update the proto comment if buffering is kept |
| IPC-04 | Yes, after a consumer exists | new IR schemas and an inline-versus-reference rule |
| IPC-05 | Yes (amendment to ADR 0048) | alternatives text lacks evidence |
| IPC-06 | No | comment and doc correction under ADR 0048 B9 |
| IPC-07 | No | |
| IPC-08 | No | |
| IPC-09 | No | hardening and optional feature flag |

## 8. Open questions

- Which workload, if any, produces payloads above a few MiB between processes? Without it IPC-04 and IPC-08 stay plans.
- Should `docs/research/protocols-schemas-serialization.md` be refreshed or marked historical? Its counts are stale (see evidence file).

Back to the [index](README.md).
