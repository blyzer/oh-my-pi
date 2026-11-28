# Serialization reference repositories: schema evolution, validation, codegen, streaming

Back to the [index](../README.md). Audited commit `85d219ae7f`; worktree `/private/tmp/omp-gateway-research` (no `crates/` differences from the audited commit).

Pinned clones: tokio-rs/prost@894f4a7, hyperium/tonic@c7e68c5, capnproto/capnproto-rust@d53310e, google/flatbuffers@5eb190f, djkoloski/rkyv@d963abc (see [ref-inventory.md](ref-inventory.md)). Untrusted input: nothing was built or run. **Depth limit:** this pass read file listings and a handful of files (named below); it did not read the repositories' schema-evolution documentation, so rows marked docs or general knowledge are not code-verified.

| Format | Schema evolution | Zero-copy and validation | Codegen | Streaming |
|---|---|---|---|---|
| prost | tag-based; `skip_field` drops unknown fields on decode (`tokio-rs/prost@894f4a7:prost/src/encoding.rs`); no retention found by name | not zero-copy; `bytes()` option maps bytes fields to `Bytes` (`tokio-rs/prost@894f4a7:prost-build/src/config.rs`); recursion limit exists | build-time, derive-based; OMP uses protox so no `protoc` | caller frames messages |
| tonic | follows prost | default receive limit 4 MiB, send unlimited (`hyperium/tonic@c7e68c5:tonic/src/codec/mod.rs`); gzip and zstd behind features (`hyperium/tonic@c7e68c5:tonic/src/codec/compression.rs`) | `tonic-prost-build` | client, server and bidirectional streams |
| capnproto-rust | pointer-based, additive evolution (docs, not read here) | reader traversal limit 8 Mi words, nesting 64 (`capnproto/capnproto-rust@d53310e:capnp/src/message.rs`); messages written in segments (`capnproto/capnproto-rust@d53310e:capnp/src/serialize.rs`) | `capnpc` build step plus the `capnp` tool | async message IO in `capnp-futures` (`capnproto/capnproto-rust@d53310e:capnp-futures/src/serialize.rs`); own RPC in `capnp-rpc` |
| FlatBuffers | table fields append-only, vtables tolerate absent fields (general knowledge, not read here) | `root` runs the verifier (depth 64) and `root_unchecked` skips it (`google/flatbuffers@5eb190f:rust/flatbuffers/src/get_root.rs`, `google/flatbuffers@5eb190f:rust/flatbuffers/src/verifier.rs`) | external `flatc` binary | none in the Rust crate |
| rkyv | no evolution story comparable to tags; layout follows Rust types and crate features (README level, not read here) | access without validation is `unsafe`; a checked path uses bytecheck (`djkoloski/rkyv@d963abc:rkyv/src/api/high/checked.rs`); `SECURITY.md` present (`djkoloski/rkyv@d963abc:SECURITY.md`, not read) | derive macros, no IDL | none |

Implications for OMP2: (1) a verifier or validation call is mandatory on any untrusted zero-copy read, so zero-copy does not remove a validation pass; (2) FlatBuffers needs an external binary, which conflicts with the no-`protoc`-style build rule in `AGENTS.md`; (3) rkyv is suitable only inside one Rust build, which excludes envd, Python and external peers; (4) Cap'n Proto adds a second IDL, second compatibility regime and its own RPC; (5) Protobuf with `buf breaking` already provides mechanical tag compatibility checking. None of this is measured.

Back to the [index](../README.md).
