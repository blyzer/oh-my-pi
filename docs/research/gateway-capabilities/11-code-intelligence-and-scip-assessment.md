# Shared code intelligence and SCIP assessment

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 11. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: first pass, written under a request budget, so several areas are labelled `not examined` or `not measured` instead of guessed. Evidence: [omp2-code-intelligence.md](evidence/omp2-code-intelligence.md) and [omp2-code-intelligence.tsv](evidence/omp2-code-intelligence.tsv) (rows C01 to C20), [ref-code-intelligence.md](evidence/ref-code-intelligence.md), matrices [SCIP](evidence/matrix-scip-code_scip.tsv), [Glean](evidence/matrix-facebookincubator_Glean.tsv), [Tantivy](evidence/matrix-quickwit-oss_tantivy.tsv). Diagrams: [16](diagrams/16-shared-code-intelligence-layer.md), [17](diagrams/17-scip-integration.md). Code was read in `/private/tmp/omp-gateway-research` (paths under `crates`, `docs/adr`, `docs/knowledge-base` equal `85d219ae7f`).

## 1. Answer in one paragraph

OMP2 already has the right core: `omp-kb` imports SCIP from rust-analyzer, scip-typescript and scip-python, binds the result to a Git revision and a file-hash fingerprint, refuses a build when `HEAD` or a file moves under the producers, and marks tree-sitter fallbacks as `syntactic`. Around it sit native grep, tree-sitter and ast-grep tools, an LSP tool behind the Environment, and Mnemopi for conversation memory. What is missing is not an indexer. It is (1) any agent-facing use of the index (only `omp kb` calls it), (2) a way for swarm children to share it (the cache is per Git worktree and child views have no `.git`), (3) a result contract that names workspace identity, revision and per-relation provenance, and (4) measurements. SCIP export, Glean and Tantivy are not needed on current evidence. The four named IRs do not exist in code; only parked design notes use them.

## 2. Principle tests

| Principle | Result | Evidence |
|---|---|---|
| Reuse existing language SCIP indexers | Holds. Three are run; the SCIP README lists seven more (scip-java, scip-clang, scip-ruby, scip-dotnet, scip-dart, scip-php, debian-lsp) that could be added as producer table rows. | `crates/kb/src/producer.rs:55`; `scip-code/scip@8fe5037:README.md` |
| Never derive authoritative symbol relations from embeddings, text or an LLM | Holds in kb, but only per symbol: `Resolution::Syntactic` (`graph.rs:39`). Call and implementation edges come from tree-sitter, not SCIP, and are not marked per edge. Mnemopi and ADR 0060 never write relations. | `graph.rs:297`, CIX-05 |
| Share across swarm agents with workspace identity and revision consistency | Does not hold today. kb is per Git worktree, unusable in `.git`-less child views, and used by no agent. Memory banks and LSP are shared via the Environment. | `store.rs:105`, ADR 0007, `bank.rs:75`, CIX-03 |

## 3. Layers: lexical, structural, semantic, associative

Definitions used here: **lexical** answers "which text matches", **structural** answers "what syntax shape is here", **resolved** answers "which symbol is this and who relates to it" (compiler-grade, authoritative), **semantic** answers "what is probably about this meaning" (embedding or judge, advisory), **associative** answers "what was linked to this before" (memory, advisory). The owner's four categories map to these; resolved is a refinement of structural that carries authority.

| Layer | Question | Authority | Existing (label) | Missing |
|---|---|---|---|---|
| Lexical | text and path match | exact for text | `crates/grep` (Implemented), glob and walker, `extensions/fuzzy-index` trigrams (Partial) | nothing shown (CIX-07, CIX-09) |
| Structural | syntax shape, summaries, rewrite | exact for syntax | `crates/ast`, `ast_grep`, `ast_edit` (Implemented) | nothing (CIX-07) |
| Resolved | definition, references, implementations, calls | authoritative only when `scip`; call edges syntactic | `omp-kb` (Implemented, CLI only); LSP live (Implemented) | agent surface, sharing, edge provenance (CIX-02, 03, 05) |
| Semantic | meaning-based file or range candidates | advisory | ADR 0060 (Proposed, not started); memory embeddings (Partial) | decided by a spike (CIX-10) |
| Associative | prior links among remembered facts | advisory | Mnemopi graph and episodic voices (Implemented for conversations); ADR 0062 (Proposed) | code is not in the banks (CIX-10) |

Rule that follows: results flow up the table only as candidates. A semantic or associative result may select where to look; a resolved fact must be re-obtained from a resolved or lexical layer before an edit relies on it.

## 4. Do the typed IRs exist?

No. `CodeQueryIR`, `CodeIR`, `ChangeIR`, `VerificationIR`: zero hits in any `.rs`, `.proto`, `.py` or `.kdl` file at the audited commit. They appear in `docs/parked/2026-10-06-p14-p16-architecture-notes.md` ("parked, not scheduled", not authoritative; it uses `QueryIR`, not `CodeQueryIR`) and in earlier research notes that record the same negative. Commands and neighbours are in [omp2-code-intelligence.md](evidence/omp2-code-intelligence.md) section 1. The brief's "existing Code Semantic Layer" has no named counterpart in code; the existing layer is the set of modules in section 3. Labels: C14 `Absent`; C15 to C17 `Proposed`. Recommendation: CIX-11 (one result contract) and CIX-12 (no four-IR build).

## 5. Verified current state

- **Callers of omp-kb.** Only `omp kb` (`crates/app/src/kb_cmd.rs:52`, `cli.rs:1231`, `:3362`) and the standalone `omp-kb` binary. No tool, Director, hook or extension. ADR 0060 (`:30`) agrees.
- **Revision.** `Snapshot { revision, fingerprint, files }` (`store.rs:20`). A build is refused if `HEAD` changed (`error.rs:31`) or any file changed while producers ran. `stale()` re-hashes the file set.
- **Document version.** Source is re-read from disk at decode (`graph.rs:521`); SCIP `Document.text` is not used. Position encodings UTF-8/16/32 are honoured (`graph.rs:174`).
- **Invalidation.** Whole-index rebuild, detached after `refresh` (`store.rs:306`); a marker file makes the index stale until `update` finishes.
- **Subagents.** Children neither share nor rebuild an index; they cannot run kb (no `.git` in the view) and nothing calls it.
- **Memory.** Mnemopi is conversation memory with FTS5, vectors and links fused by RRF; it does not index code (ADR 0062 Proposed). ADR 0060 is Proposed, not started. ADR 0053 is Partially implemented and bounds model judgment to acceptance.
- **Doc drift.** `crates/kb/README.md` links a heading that does not exist; `crates/kb/src/main.rs:13` says "Rust" though three languages are indexed.

## 6. SCIP import and export attach points

Import (exists): producers write a `.scip` file to a temp file in the cache dir (`store.rs:186`), `read_scip` parses it, documents are merged, `decode` builds the `Index`. Attach points to extend, all in `crates/kb`: a `PRODUCERS` row per added indexer (`producer.rs:55`); use of `Import`, `ReadAccess`, `WriteAccess`, `Test` and `Generated` role bits and `is_type_definition`, none read today (`graph.rs:534`); per-document reuse (CIX-06); coverage reporting (CIX-14).

Export (absent): the only plausible points are (a) retaining each producer's bytes unmodified (CIX-04, deferred until a consumer exists) and (b) a faithful re-encode, rejected because call edges and `syntactic` symbols have no SCIP field and would be misrepresented. `scip` 0.10.0 already provides `write_message_to_file`, so cost is small if ever needed. The format is a transmission format and says so (`docs/DESIGN.md`): it is not a storage or query format, which matches omp-kb's design of importing into a private index. SCIP has no revision field (`scip.proto:41`), so revision binding stays the consumer's job.

## 7. Glean, Tantivy, SCIP tooling verdicts

| Item | Verdict | Reason |
|---|---|---|
| Glean | `Unnecessary or redundant` (CIX-08) | Haskell and C++ stack of about 107k and 52k lines, Linux-tested build, hsthrift, folly, RocksDB; solves multi-repository fact storage that OMP2 does not have. It ingests SCIP, so an external feed remains possible later. Licence file is BSD 3-clause text under a non-standard heading, which explains GitHub's NOASSERTION; not a blocker. |
| Tantivy | `Unnecessary or redundant` for now (CIX-09) | Good fit as a Rust library (MIT, mmap, immutable segments, reload policy), but no measured gap in grep, and it is not a trigram or symbol engine. Reconsider after B3. |
| `scip` CLI (`lint`, `print`, `stats`, `snapshot`) | `Suitable as an external integration` | Useful for validating producer output in CI or debugging; not a runtime dependency. Not run here. |
| Sourcegraph-listed indexers beyond the three | `Suitable as an external integration` | Same mechanism as today's producers; add per language demand. |

## 8. Benchmark plan (`not measured`)

Nothing in this deliverable was measured. Environment at drafting, for context only: `arm64`, 10 cores, 16 GiB, load average about 18 (other work was running), so any number taken then would have been unreliable. Every item below is `not measured`; each needs the exact command, environment, raw output and run count in `evidence/measurements/NN-<slug>.md`.

| ID | Question | Method | Pass or decision rule |
|---|---|---|---|
| B1 | Cold and warm `omp kb update` time per producer, index size, peak RSS | `omp kb update` on this repository and on `crates/kb/tests` fixtures; 5 runs; time and `/usr/bin/time -l` | Decides CIX-06 |
| B2 | Query latency: `omp kb query` process (load whole JSON) versus an in-process cached `Index` | 30 queries of mixed name, repeated; wall time and RSS | Decides CIX-02 shape |
| B3 | Lexical baseline: grep, `fuzzy-index`, Mnemopi FTS5, and Tantivy only if the first three miss a latency budget set before the run | Fixed query set, warm and cold cache | Decides CIX-09 |
| B4 | N children: rebuild per child versus shared read-only index (prototype) | 4 and 8 children, cold | Decides CIX-03 |
| B5 | `Store::stale` cost on a large tree (hashes every file) and one-file-edit update | Edit one file, time stale check and refresh | Decides CIX-06 |
| B6 | Semantic find quality and cost | ADR 0060 D1 spike | Decides CIX-10 |
| B7 | Precision of `syntactic` call edges against SCIP-resolved ones where both exist | Compare on the Rust fixture and a TypeScript fixture | Decides CIX-05 labelling |

## 9. Draft ADR needed?

Yes, one: "Shared code index: workspace identity, revision key and result contract" covering CIX-03 and CIX-11 (and the labelling of CIX-05). Reasons: it changes the identity contract between `crates/kb`, envd snapshot identity and the subagent spawner, and it touches ADR 0007 (child views exclude `.git`) and ADR 0049 placement. No ADR for Glean, Tantivy, SCIP export or the IRs: the cards reject or defer them. CIX-02 needs an ADR only if it asks for T1 placement. The ADR is not drafted in this pass.

## 10. Capability cards

### CIX-01 Keep SCIP import through external producers as the only source of resolved symbol relations
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `crates/kb/src/producer.rs:55 PRODUCERS` (rust-analyzer, scip-typescript, scip-python), `crates/kb/src/graph.rs:408 Index::read_scip`, `store.rs:168 Store::build`, tests `crates/kb/tests/{cli,languages}.rs` (Implemented, reachable from `omp kb`).
**Actual missing capability:** None for the principle. Gaps are listed in CIX-02, CIX-03 and CIX-12.
**Alternatives considered:** Write OMP2 resolvers per language (rejected: duplicates rust-analyzer and the Sourcegraph indexers); derive relations from embeddings or an LLM (rejected by the owner brief and ADR 0053); use LSP only (not durable, per-server startup, `docs/knowledge-base/README.md` tier 3).
**Expected benefit:** Reliability and security: resolved relations come from compilers' own front ends; no new inference computation.
**Implementation complexity:** low (no change)
**Security implications:** Producers are external processes started with `Command::new` and `stdin`/`stdout` null (`producer.rs:95`); output is path-validated (`UnsafePath`, `graph.rs:248`) and only read, never executed. Env overrides (`RUST_ANALYZER`, `OMP_KB_SCIP_*`) let the environment choose the binary, which is a trust input.
**Maintenance cost:** low; three producer command lines to keep current with upstream flags.
**Performance measurement strategy:** not applicable
**Acceptance criteria:** The existing `crates/kb/tests` stay green; a relation with `resolution: scip` is only ever produced from producer output.

### CIX-02 Give agents a read-only query surface over the shared index
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `omp kb query` (`crates/app/src/kb_cmd.rs:62`) returns JSON with `revision`, `fingerprint`, `stale`, `miss`, `results` (`graph.rs:115 Query`); `docs/adr/0060-semantic-find.md:30` records the CLI as the only consumer (Implemented as a CLI, `Absent` as a tool).
**Actual missing capability:** No tool, device or Director calls `Store`, so an agent cannot ask for definitions, references, callers or implementations except by running a shell command. `load` re-parses the whole JSON per call (`store.rs:142`).
**Alternatives considered:** Keep CLI only and tell agents to run `omp kb query` (works today in the parent workspace, not in child views, see CIX-03); expose through the existing LSP tool (LSP has live results but no durable cross-crate index); a new daemon (rejected: second control plane).
**Expected benefit:** Latency and reliability of symbol lookups; fewer text-search tokens spent finding callers. Magnitude `not measured`.
**Implementation complexity:** medium: a tool or device row under ADR 0049 placement rules (default T2, T1 only with evidence), a typed result, rendering, an in-process cached `Index`.
**Security implications:** Read-only. The tool must not start producers implicitly; `update` runs `rust-analyzer`, which executes build scripts and proc macros (`cli.rs:52` comment), so it needs Exec-tier approval like any process.
**Maintenance cost:** medium; one more tool contract and result schema.
**Performance measurement strategy:** B2 and B3 in section 8.
**Acceptance criteria:** A query tool returns the same fields as `omp kb query`, adds the workspace identity of CIX-03, never runs a producer unless `update` is requested, and reports `stale: true` instead of silently answering from an old index.
**Follow-up:** Requires benchmarking before promotion beyond T2 (ADR 0049 D1).

### CIX-03 Share one index across swarm children by workspace identity and revision key
**Decision class:** Missing and justified: propose
**Existing implementation evidence:** Cache path is `git rev-parse --git-path omp-kb` (`store.rs:105`), per Git worktree (checked). Child views exclude `.git` (`docs/adr/0007-subagent-filesystem-isolation.md`) and live under the envd state directory (`crates/envd/src/workspace/operations.rs:694`); `git rev-parse --show-toplevel` fails there (checked in a temp directory). Memory already keys on the canonical primary root (`crates/memory/src/bank.rs:75 BankScope::resolve`) (Absent for kb).
**Actual missing capability:** A key that names the index independent of Git: (canonical primary root, base snapshot id and manifest hash from envd, producer name and version). Children open the parent's index read-only through the Environment, and an overlay for files they changed, labelled `syntactic` until the parent reindexes after merge.
**Alternatives considered:** Each child runs `omp kb update` (rejected: needs `.git`, repeats minutes of producer work, `omp-kb` is large, `docs/research/grammar-bundling.md:41`); copy `.git` into every view (rejected by ADR 0007, which excludes it); one global server (rejected: second control plane, ADR 0006 host authority).
**Expected benefit:** Latency and memory: one producer run per base revision instead of one per child; reliability: all children answer from the same revision. Magnitude `not measured`.
**Implementation complexity:** high: spans `crates/kb`, `crates/envd` snapshot identity and the subagent spawner; needs an ADR.
**Security implications:** The index describes source text; a child must not be able to write the shared index or the parent's. Reads go through the Environment authority; paths stay relative and validated (`UnsafePath`).
**Maintenance cost:** medium; a new identity contract to keep consistent with `DurableSnapshotRecord`.
**Performance measurement strategy:** B4 in section 8.
**Acceptance criteria:** Two children created at snapshot S receive the same index digest; a child edit never changes the parent's index; a query in a child names base snapshot, overlay file count and whether any overlay file is `syntactic`; a stale base is refused or flagged exactly as `RevisionChangedDuringProducers` is today.
**Follow-up:** Draft ADR needed (section 9).

### CIX-04 Do not write a SCIP exporter for the derived index
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** No SCIP writer exists (`git grep write_to crates/kb` empty). The `scip` crate 0.10.0 already provides `write_message_to_file` (`scip-code/scip@8fe5037:bindings/rust/src/mod.rs`). `Store::build` reads each producer's SCIP from a temporary file and merges it into one `combined` index (`crates/kb/src/store.rs:186-196`); `Index::decode` (`crates/kb/src/graph.rs:423`) derives the JSON from the merged index; the raw producer SCIP is not persisted, so an exporter would have to retain or write the merged `combined` index.
**Actual missing capability:** Nothing is missing that an exporter would supply. The raw producer SCIP is the exportable artifact; the derived JSON adds only syntactic call edges that SCIP has no field for, and exporting syntactic data as SCIP would blur provenance.
**Alternatives considered:** Re-encode the derived index as SCIP (rejected: lossy and mislabels `Resolution::Syntactic`); keep each producer's `.scip` as a content-addressed artifact (cheap, see follow-up); feed Glean or Sourcegraph (external, no current consumer).
**Expected benefit:** None identified; avoids code and a provenance risk.
**Implementation complexity:** low if ever done (retain bytes), high if a faithful exporter were written
**Security implications:** A retained `.scip` file contains paths and symbol names of the repository; it needs the same access class as the source.
**Maintenance cost:** none now
**Performance measurement strategy:** not applicable
**Acceptance criteria:** No exporter is added; if a consumer appears, `omp-kb` stores the producer bytes unmodified beside the object and the digest covers them.
**Follow-up:** Revisit only when a named consumer (for example Glean, CIX-08) is chosen; then persist producer bytes, do not synthesize SCIP.

### CIX-05 Make provenance a contract on every relation: authoritative, syntactic or associative
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** `Resolution::{Scip, Syntactic}` per symbol (`crates/kb/src/graph.rs:39`), `syntactic:` ids (`graph/syntax.rs:73`), query `source` becomes `syntactic` when all hits are syntactic (`graph.rs:648`) (Implemented in kb only).
**Actual missing capability:** The tag is per symbol, not per edge: call edges and implementation edges from tree-sitter (`graph.rs:297`) sit on SCIP symbols without being marked. There is no tag for semantic or associative candidates (ADR 0060 `locate`, Mnemopi).
**Alternatives considered:** Trust the consumer to know (rejected: invites treating a call edge as compiler-resolved); drop syntactic data (rejected: the fallback keeps Python and JavaScript useful without producers, `tests/languages.rs`).
**Expected benefit:** Reliability: an agent can see which relations may be relied on for edits. No token or latency effect.
**Implementation complexity:** low to medium: add an edge-level origin to `Call` and `Implementation` and carry it in `Query`.
**Security implications:** Prevents a model from acting on embedding or text-derived edges as if compiler-resolved.
**Maintenance cost:** low
**Performance measurement strategy:** not applicable
**Acceptance criteria:** Every relation in a query result names its origin (`scip`, `syntactic`, `associative`); an associative origin can never be emitted by `omp-kb`; a test asserts a call edge found only by tree-sitter is not labelled `scip`.

### CIX-06 Document-level incremental invalidation instead of a full rebuild
**Decision class:** Requires benchmarking
**Existing implementation evidence:** `Store::refresh` (`store.rs:306`) validates paths then runs a full detached `update`; `Store::stale` hashes every file (`store.rs:110`); `crates/ast/src/parse_cache.rs:1` is content-addressed per file (Partial).
**Actual missing capability:** No per-document reuse of SCIP output; SCIP's schema is per Document (`scip.proto:76`) so a producer's unchanged documents could be kept if the producer supports partial runs, which was not checked for the three producers.
**Alternatives considered:** Keep full rebuild plus detached refresh (works, cost unmeasured); run producers per package or crate (producer-dependent); watch file events and mark documents dirty (more moving parts).
**Expected benefit:** Latency: updates after an edit. Magnitude `not measured`; the only figure in the repo is a code comment (`tests/cli.rs:52`).
**Implementation complexity:** medium to high, producer-dependent
**Security implications:** No new surface beyond CIX-02.
**Maintenance cost:** medium
**Performance measurement strategy:** B1 and B5 in section 8.
**Acceptance criteria:** The benchmark reports cold build, no-change staleness check and one-file-edit update time on this repository and on a small fixture; a design change is accepted only if the one-file update is measurably cheaper than `update`.
**Follow-up:** If the benchmark shows full rebuild dominates, extend; otherwise keep.

### CIX-07 Reuse the lexical and structural layers as they are
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `crates/grep/src/lib.rs` (`GrepSink`, `grep_stream`), `crates/ast/src/{parse_cache,summary,symbol,ops}.rs`, `crates/tools/src/ast_grep.rs:417`, `ast_edit.rs`, `extensions/fuzzy-index` (Implemented; trigram is a Python extension, Partial).
**Actual missing capability:** None.
**Alternatives considered:** Add a persistent lexical index (see CIX-09); add a second tree-sitter wrapper (rejected: `omp-ast` already owns parsing and `omp-kb` depends on it, `crates/kb/Cargo.toml:18`).
**Expected benefit:** Memory and complexity avoided.
**Implementation complexity:** low (no change)
**Security implications:** Existing policies on path scope and edit approvals apply.
**Maintenance cost:** low
**Performance measurement strategy:** not applicable
**Acceptance criteria:** No new crate or dependency is added for lexical or structural search by this research.

### CIX-08 Do not adopt Glean
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** No Glean code or dependency (`Cargo.lock`); `docs/architecture/p1-capability-inventory.md:68` marks Glean/Angle absent in both trees, REMOVE. Reference: `facebookincubator/Glean@92ba871`, Haskell 107k lines, C++ 52k lines, Linux-only tested build with `hsthrift`, folly and RocksDB (`building.md`).
**Actual missing capability:** None for OMP2's workload (one repository or worktree, one session). Glean's strengths are very large multi-repository fact stores and Angle queries.
**Alternatives considered:** Adopt Glean as a server (rejected: build weight and a second service tier against ADR 0049 and the owner's lightweight-harness principle); run Glean externally fed by SCIP (possible, but no consumer needs it); keep `omp-kb` (chosen).
**Expected benefit:** Avoided: a Haskell and C++ dependency graph and a new process tier.
**Implementation complexity:** high if ever adopted
**Security implications:** A resident database service would hold the whole codebase; licence is BSD 3-clause text under a non-standard heading, not the blocker.
**Maintenance cost:** high (separate toolchain)
**Performance measurement strategy:** not applicable
**Acceptance criteria:** No Glean code, build step or service appears in the OMP2 tree.
**Follow-up:** Reconsider as a Suitable as an external integration only if cross-repository queries at monorepo scale become a stated requirement; Glean already ingests SCIP, so CIX-04's retained bytes would be the interface.

### CIX-09 Do not add Tantivy now; decide with a measured grep baseline
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** Absent (no entry in `Cargo.lock`). Existing: streaming grep, Python trigram extension, SQLite FTS5 in Mnemopi (`crates/memory/src/store.rs:1718`). Reference: `quickwit-oss/tantivy@c005b68` v0.27.0, MIT, mmap directory, immutable segments, `ReloadPolicy`.
**Actual missing capability:** Not shown to be missing: no measurement says repository grep is too slow for an agent turn. Tantivy would also not give regex or trigram semantics without custom tokenization.
**Alternatives considered:** Tantivy in `crates/kb` or a T1 crate (adds `memmap2`, `lz4_flex`, `zstd`, `rayon` and a 1.91 Rust floor to a binary whose cold build time is a gate, `AGENTS.md` locked decisions); extend `fuzzy-index` (T2, already present); SQLite FTS5 (already a dependency).
**Expected benefit:** None identified now.
**Implementation complexity:** medium if adopted
**Security implications:** An on-disk index of source text copies code into another store; it needs the same location and permission policy as the omp-kb cache.
**Maintenance cost:** medium
**Performance measurement strategy:** B3 in section 8 compares grep, the trigram extension, FTS5 and, only if grep fails the budget, Tantivy.
**Acceptance criteria:** No new search dependency is added unless B3 shows a named latency budget is missed by existing mechanisms.
**Follow-up:** If B3 fails, the next class is Suitable as an optional extension, behind a feature, mmap-shared among children.

### CIX-10 Keep semantic and associative results advisory and separate from authority
**Decision class:** Requires benchmarking
**Existing implementation evidence:** Mnemopi four-voice recall (`crates/memory/src/recall.rs:34`) for conversation memory (Implemented); embedding worker (`crates/memory/src/embedding/supervisor.rs:50`); ADR 0060 `locate` Proposed, not started, no index; ADR 0062 Proposed; ADR 0053 independent verdicts Partially implemented.
**Actual missing capability:** No code-chunk or symbol embedding index and no reranker (`crates/memory`: no rerank hit). ADR 0060's own spike (D1) decides whether semantic file search ships.
**Alternatives considered:** Embed code into Mnemopi banks (rejected for authority: embeddings may nominate candidate files, never relations); a model judge for symbol edges (rejected by the owner brief and ADR 0053); skip semantic search (possible, but ADR 0060 exists to test it).
**Expected benefit:** Possible recall improvement for 'where is the code that does X'. Magnitude `not measured`.
**Implementation complexity:** medium
**Security implications:** Retrieved text goes to model context; candidates must be rechecked against the SCIP or grep layer before any edit.
**Maintenance cost:** medium
**Performance measurement strategy:** B6 in section 8, following ADR 0060 D1's spike.
**Acceptance criteria:** A semantic hit is returned as a file or range candidate with origin `associative`; any relation shown with it comes from CIX-01 or is marked `syntactic`; no edit tool accepts an `associative` origin as proof.
**Follow-up:** Follow ADR 0060 D1; do not duplicate it here.

### CIX-11 Add one typed query result contract, not four IRs
**Decision class:** Missing and justified: propose
**Existing implementation evidence:** `crates/kb/src/graph.rs:115 Query` is an untyped-to-callers JSON shape; CodeQueryIR is `Absent`, CodeIR, ChangeIR, VerificationIR are `Proposed` in `docs/parked/2026-10-06-p14-p16-architecture-notes.md:1178` and nowhere in code.
**Actual missing capability:** A small serde type (a `CodeQueryResult`) carrying workspace identity, revision, fingerprint, per-relation origin and staleness, used by the CLI, a future tool (CIX-02) and children (CIX-03). It is the only part of the IR vocabulary a current consumer needs.
**Alternatives considered:** Implement the full chain (rejected as speculative; parked notes are not authoritative); leave JSON ad hoc (works for the CLI, fails once a tool and children consume it); reuse protobuf (possible, see `docs/research/gateway-capabilities/06b-ipc-and-serialization.md`).
**Expected benefit:** Reliability: one schema for all consumers.
**Implementation complexity:** low to medium
**Security implications:** As CIX-05.
**Maintenance cost:** low
**Performance measurement strategy:** not applicable
**Acceptance criteria:** One crate-owned type is serialised by the CLI and a tool; golden tests pin the JSON; the type has no field for model-derived relations.
**Follow-up:** Draft ADR needed (section 9), folded into the CIX-03 ADR.

### CIX-12 Do not build CodeIR, ChangeIR or VerificationIR as new layers
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** ChangeIR neighbours: `crates/envd/src/workspace/operations.rs:789 merge_worktree` (manifest diff artifact) and content-addressed child patches (ADR 0007). VerificationIR neighbour: ADR 0053 `Verdict` (STATUS row, Partially implemented). CodeIR: `omp-kb::Index` and `omp-ast` summaries (Partial).
**Actual missing capability:** No consumer needs a named IR above the existing artifacts; the gap is the result contract of CIX-11.
**Alternatives considered:** Build the chain from the parked notes (rejected: they say parked, not scheduled); rename existing types to match (churn without benefit).
**Expected benefit:** Avoided: four contracts to keep consistent.
**Implementation complexity:** not applicable
**Security implications:** none
**Maintenance cost:** none
**Performance measurement strategy:** not applicable
**Acceptance criteria:** No `CodeIR`, `ChangeIR` or `VerificationIR` type is added; deliverables that need them cite the existing artifacts.
**Follow-up:** Reopen only when two crates need to exchange a structure that no existing artifact carries.

### CIX-13 Treat LSP as the live tier behind the Environment, not as the index
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `crates/tools/src/lsp/mod.rs:354 LspControl` and `:365 LspTool`, modules `navigation.rs`, `symbol.rs`, `diagnostics.rs`, `refactor.rs`; `docs/knowledge-base/README.md` tier 3 (Implemented; server pooling not examined).
**Actual missing capability:** None to add; document that LSP results are live and unrevisioned against kb's fingerprint.
**Alternatives considered:** Make LSP the only code index (rejected: not durable, per-language server start cost); duplicate LSP in kb (rejected).
**Expected benefit:** Accuracy for just-edited files.
**Implementation complexity:** low
**Security implications:** Servers run project code analysis in the Environment; existing approvals apply.
**Maintenance cost:** low
**Performance measurement strategy:** not applicable
**Acceptance criteria:** A tool result states whether it came from `live` LSP or an indexed fingerprint; no change to LSP code required.

### CIX-14 Report absent producers and file coverage in every query
**Decision class:** Suitable as an external integration
**Existing implementation evidence:** `Run::Absent` is silently skipped (`producer.rs:106`); uncovered files get syntactic data (`store.rs:216`); producers are external executables found by name or env var (`producer.rs:96`).
**Actual missing capability:** A query result does not say which producers ran, which were absent, or which files fell back; scip-typescript and scip-python must be installed by the user.
**Alternatives considered:** Bundle producers (rejected: heavy and version-locked); fail the build on absent producers (rejected: breaks the fallback); report coverage (chosen).
**Expected benefit:** Reliability: the agent knows how far to trust the index.
**Implementation complexity:** low
**Security implications:** Producer discovery by PATH is an existing trust input; report the resolved path, not file contents.
**Maintenance cost:** low
**Performance measurement strategy:** not applicable
**Acceptance criteria:** `Query` or its successor lists producers (name, version, status) and counts of SCIP-covered and syntactic files for the revision.

## 11. Not examined

LSP server lifecycle and pooling across children; the `feat/mnemopi-fact-extraction` branch; whether the three producers support partial runs; grammar set linked into `omp-kb` beyond the recorded 97.3 MB figure; SCIP `scip.md` generated reference; Glean schema and server code; any timing.

Back to the [index](README.md).
