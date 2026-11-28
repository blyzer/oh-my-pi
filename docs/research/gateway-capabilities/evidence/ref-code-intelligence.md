# Reference repositories: code intelligence

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence for deliverable 11. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Pinned clones in `/private/tmp/omp-research-refs/local/` (SHAs from [ref-inventory.md](ref-inventory.md)): `scip-code/scip@8fe5037` (Apache-2.0), `facebookincubator/Glean@92ba871` (GitHub says NOASSERTION), `quickwit-oss/tantivy@c005b68` (MIT). Nothing from them was built or run. TSV twins: [matrix-scip-code_scip.tsv](matrix-scip-code_scip.tsv), [matrix-facebookincubator_Glean.tsv](matrix-facebookincubator_Glean.tsv), [matrix-quickwit-oss_tantivy.tsv](matrix-quickwit-oss_tantivy.tsv).

## 1. scip-code/scip@8fe5037

What was read: `README.md`, `docs/DESIGN.md`, `docs/CLI.md`, `scip.proto` (962 lines), `bindings/rust/{Cargo.toml,src/mod.rs,src/symbol.rs}`, file list of `cmd/scip`.

- **Role of the format.** `docs/DESIGN.md`: SCIP "is meant to be a _transmission_ format for sending data from some producers to some consumers -- it is not meant as a _storage_ format for querying". Non-goals include code modification, ease of writing consumers, and "efficient code navigation by itself" (needs a query engine). This matches the OMP2 pattern: import SCIP, derive a private query index (`omp-kb`).
- **Schema** (`scip.proto`): `Index { metadata, documents, external_symbols }` (`:26`); `Metadata { version, tool_info, project_root, text_document_encoding }` (`:41`), no commit or revision field; `Document { language, relative_path, occurrences, symbols, text, position_encoding }` (`:76`); `Symbol { scheme, package{manager,name,version}, descriptors }` with a string grammar (`:146` to `:190`), `local <id>` symbols scoped to one Document; `Descriptor` suffixes Namespace, Type, Term, Method, TypeParameter, Parameter, Meta, Local, Macro (`:205`); `SymbolInformation { symbol, documentation, relationships, kind, display_name, signature_documentation, enclosing_symbol }` (`:253`); `Relationship { symbol, is_reference, is_implementation, is_type_definition, is_definition }` (`:465`), with the documented example that `Dog#sound()` implements and references `Animal#sound()`; `SymbolRole` bitset Definition 0x1, Import 0x2, WriteAccess 0x4, ReadAccess 0x8, Generated 0x10, Test 0x20, ForwardDefinition 0x40 (`:524`); `Occurrence { range / typed_range, symbol, symbol_roles, override_documentation, syntax_kind, diagnostics, enclosing_range }` (`:692`); `PositionEncoding` UTF-8, UTF-16, UTF-32 (`:122`).
- **Indexers** (README, "Tools using SCIP"): scip-java (Java, Scala, Kotlin), scip-typescript, rust-analyzer, scip-clang (C++, C), scip-ruby, scip-python, scip-dotnet, scip-dart, scip-php, debian-lsp. OMP2 already runs three of them.
- **Tooling.** `scip lint`, `print --json`, `snapshot`, `stats`, `test`, and an experimental `expt-convert` to SQLite (`docs/CLI.md`). These are useful external validators for producer output; none is needed at runtime.
- **Rust binding.** `bindings/rust/Cargo.toml`: crate `scip` 0.10.0, Apache-2.0, `protobuf =3.7.2`; `src/mod.rs` has `write_message_to_file`; `src/symbol.rs` has `parse_symbol`, `format_symbol`, `is_local_symbol`.
- **Limits for OMP2.** No revision field, so the consumer must bind each index to a revision (omp-kb does, `Snapshot`). A SCIP file carries no calls graph; call edges need a separate source.

## 2. facebookincubator/Glean@92ba871

What was read: `README.md`, `LICENSE` (whole text), `cabal.project`, `Dockerfile`, `install_deps.sh`, `glean.cabal.in` library list, `glean/website/docs/building.md` (first 3,200 characters), directory listings.

- **What it is.** "A system for working with facts about source code", queried with the Angle language (README). Full indexers for C++/C, Hack, Haskell, JavaScript/Flow, Python; SCIP/LSIF ingestion for Rust (rust-analyzer), Go, TypeScript, Java, Python, Dotnet. Directories `glean/lang/scip` and a Rust `scip_to_glean` tool exist (not read).
- **Weight.** Census by file extension under the clone (my command: a Python `os.walk` count): 15,540 files; `.hs` 543 files / 107,170 lines, `.cpp` 283 / 34,311, `.h` 142 / 18,073, `.angle` 102 / 21,465, `.thrift` 30 / 6,496. Working tree 155,840 KB (the large share is `.out`, `.perf`, `.query` test data: 7,051 + 4,836 + 2,004 files).
- **Build needs.** Haskell (GHC 9.6.7 and Cabal via ghcup, `building.md`), `hsthrift` cloned and built by `install_deps.sh` (also builds RocksDB unless system libs are used), folly, fbthrift stack, a C++ toolchain with cmake and ninja, dozens of system packages (boost, glog, gflags, rocksdb, libsodium and others). `building.md`: "The build is only tested on Linux". `glean.cabal.in` `extra-libraries` include folly, rocksdb, fmt, stdc++. Optional clang-19 for the C++ indexer.
- **Licence.** The `LICENSE` file (1,532 bytes) is titled "BSD License / For Glean software", "Copyright (c) Facebook, Inc. and its affiliates", with three conditions (retain notice, reproduce notice in binaries, no endorsement by the Facebook name) followed by the standard "AS IS" disclaimer. That is the BSD 3-clause text with a non-standard heading, which is why automatic detection returns NOASSERTION. No separate patent file is in the root listing. This is a reading, not legal advice; the README says "licensed under a BSD LICENSE". Licence is therefore not the blocker.
- **Process model.** `glean-server` and a Thrift service (Dockerfile builds `glean`, `glean-server`, `glean-hyperlink`); storage is RocksDB or LMDB (`glean/rocksdb`, `glean/lmdb`). Revision semantics (databases, stacking) were not read.

## 3. quickwit-oss/tantivy@c005b68

What was read: `README.md` feature list, `Cargo.toml` (version, dependencies), `ARCHITECTURE.md` headings and grep hits for segment and mmap, `src/reader/mod.rs` ReloadPolicy lines.

- Version 0.27.0, MIT, `rust-version = "1.91"`. Dependencies include `memmap2`, `fs4`, `lz4_flex`, `zstd`, `rayon`, `crossbeam-channel`, `census`, `tantivy-fst`, workspace crates `sstable`, `columnar`, `stacker`, `ownedbytes`, `bitpacker`.
- **Segments.** "A tantivy index is a collection of smaller independent immutable segments", each identified by a UUID (`ARCHITECTURE.md:41`). Deletes and merges are described in the same document.
- **mmap.** "The data is read straight from an mmapped file, and loading an index is as fast as mmapping its files" (`ARCHITECTURE.md:136`); `MmapDirectory` and `RamDirectory` (`:98`). README: "Tiny startup time (<10ms)".
- **Reload.** `ReloadPolicy::{Manual, OnCommitWithDelay}` (`src/reader/mod.rs:21`); default `OnCommitWithDelay` (`:63`); a deterministic view of a just-made commit needs `Manual` plus an explicit reload (`:36`).
- **Fit for OMP2.** It is a general inverted-index library with BM25 and a query language. It does not give trigram or regex-over-source search, symbol identity or revision binding; those would be built above it. OMP2 already has a streaming grep, a Python trigram extension, and SQLite FTS5 in Mnemopi.

Back to the [index](../README.md).
