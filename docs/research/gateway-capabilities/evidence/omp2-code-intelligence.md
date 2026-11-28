# OMP2 code intelligence audit

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence for deliverable 11. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: first pass, written under a request budget. TSV twin: [omp2-code-intelligence.tsv](omp2-code-intelligence.tsv) (rows C01 to C20). Code was read in the worktree `/private/tmp/omp-gateway-research`; `git diff --stat 85d219ae7f HEAD -- crates docs/adr docs/knowledge-base` printed nothing, so those paths equal the audited commit. Maturity labels follow [01-method-and-dimension-ids.md](01-method-and-dimension-ids.md) section 2.

## 1. Do the typed IRs exist as code?

No. How it was searched:

- `git grep -n -i -E 'CodeQueryIR|CodeIR|ChangeIR|VerificationIR|code semantic|QueryIR|DecisionIR|ReviewIR' -- . ':!docs/research/gateway-capabilities' ':!docs/parked'` returned lines only in `docs/research/native-resource-architecture/*`, `docs/research/omp2-hybrid-verification/*`, one unrelated line in `docs/research/native-cli-feasibility.md:600` and `docs/adr/0041-native-surfaces-over-tsp.md:67` ("semantic layer" in the TUI projection sense). No `.rs`, `.proto`, `.py` or `.kdl` file matched.
- A Python walk over `crates`, `docs`, `extensions`, `scripts`, `tools`, `elastic`, `examples`, `fixtures`, `.github` for `CodeQueryIR|CodeIR\b|ChangeIR|VerificationIR|semantic layer` gave 35 hits; the only code hit is `crates/ai/src/layer/semantic.rs:44` ("Creates a semantic layer"), a provider middleware layer, unrelated.
- The names live in **parked design text**: `docs/parked/2026-10-06-p14-p16-architecture-notes.md` (header: "parked, not scheduled", "Not authoritative: `PLAN.md` and production code outrank it"). It writes the chain `CodeIR -> QueryIR -> DecisionIR -> ChangeIR -> VerificationIR` (lines 1178 to 1186) and proposes adding `ReviewIR` (line 1172). The word is `QueryIR` there; the exact string `CodeQueryIR` appears only in the owner brief and in earlier research notes that say it is not established.
- Two earlier research sets reached the same negative result with their own commands: `docs/research/native-resource-architecture/01-native-baseline-and-lifecycle.md:128` and `docs/research/omp2-hybrid-verification/01-current-capability-audit.md:114`.
- "Code Semantic Layer" as a named existing architecture was not found (the case-insensitive pattern `code semantic` has no hit outside research notes). The owner brief says to "preserve and reuse" it; what exists is the set of modules in section 2.

Labels: CodeQueryIR `Absent`; CodeIR, ChangeIR, VerificationIR `Proposed` (design text only, parked). Closest implemented neighbours: `crates/kb/src/graph.rs:115 Query` (revision, fingerprint, stale, miss, results; JSON, read-only), `crates/envd/src/workspace/operations.rs:789 merge_worktree` (manifest diff artifact, three-way merge) and the ADR 0053 `Verdict` row in `docs/adr/STATUS.md` (`independent` field, Partially implemented).

## 2. Layers that exist

| Layer | Module | Evidence | Label |
|---|---|---|---|
| Lexical | native grep, glob, walker | `crates/grep/src/lib.rs` (`GrepSink`, `grep_stream`, as cited in `docs/adr/0060-semantic-find.md:34`); trigram only in `extensions/fuzzy-index` (Python, T2) | Implemented / Partial |
| Structural | tree-sitter, ast-grep | `crates/ast/src/{parse_cache,summary,symbol,ops,block}.rs`; `crates/tools/src/ast_grep.rs:417`; `ast_edit.rs` | Implemented |
| Structural, resolved | SCIP import | `crates/kb` | Implemented (CLI only) |
| Live | LSP | `crates/tools/src/lsp/mod.rs:354 LspControl` ("Application-owned bridge to the project Environment's document authority"), `:365 LspTool` | Implemented (module depth not re-read) |
| Associative | Mnemopi | `crates/memory/src/recall.rs:34 RecallVoice` | Implemented for conversation memory only |
| Semantic | embeddings | `crates/memory/src/embedding/` | Partial, no code index |
| Semantic find | ADR 0060 | `docs/adr/STATUS.md` row 0060: "Proposed 2026-11-13; reviewed (EGM); not started"; `crates/locate` and `crates/tools/src/locate.rs` do not exist | Proposed |

## 3. omp-kb, read end to end

- Producers: `crates/kb/src/producer.rs:55 PRODUCERS` has three entries. `rust-analyzer scip <root> --exclude-vendored-libraries --num-threads 2 --output <file>` (`:27`), requires `Cargo.toml` or `rust-project.json` (`:61`); `scip-typescript index --infer-tsconfig` (`:35`); `scip-python index --project-name repository --project-version 0` (`:41`). Program overridable by env (`RUST_ANALYZER`, `OMP_KB_SCIP_TYPESCRIPT`, `OMP_KB_SCIP_PYTHON`). A missing program returns `Run::Absent` (`:106`) and the build continues silently.
- Build (`store.rs:168`): snapshot before, run each applicable producer, `read_scip` (`graph.rs:408`; rejects empty or definition-less input), merge documents, snapshot after, then refuse with `RevisionChangedDuringProducers` (`error.rs:31`) if `HEAD` moved or `ChangedDuringProducers { path }` if a snapshot file changed, decode (`graph.rs:423`), add syntactic fallback for source files no producer covered (`graph/syntax.rs:174 add_syntactic`), then a last re-check (`ChangedDuringBuild`).
- What SCIP data is used: definition role bit `symbol_roles & 1` (`graph.rs:534`); `typed_range` with a fallback to legacy `range` (`:145`); `position_encoding` UTF-8/16/32 (`:174`); `SymbolInformation.relationships` `is_reference`, `is_definition`, `is_implementation` (`:492`); `external_symbols` display names (`:441`). Not used: `Import`, `WriteAccess`, `ReadAccess`, `Generated`, `Test`, `ForwardDefinition` roles, `is_type_definition`, `enclosing_range`, `Diagnostic`, `documentation`, `kind`, `Metadata`, `Document.text` (source is re-read from disk, `:521`).
- Calls and implementations are **not SCIP data**: `graph.rs:297 Syntax::visit` walks the tree-sitter tree and matches call nodes to SCIP occurrence positions. Symbols without SCIP are tagged `Resolution::Syntactic` (`graph.rs:39`, `syntax.rs:75`), and a query whose results are all syntactic reports `source: "syntactic"` (`graph.rs:648`). This is the existing provenance mechanism that the principle "never derive authoritative relations from text" needs.
- Query (`graph.rs:583`): exact name or exact symbol id match only; transitive closure over reference and definition relations (`:560`).
- Storage (`store.rs:268 update`): `Index` serialised as one JSON object, content-addressed under `<git-path omp-kb>/objects/<digest>.json`, head pointer `index.json` written by tempfile persist, exclusive file lock `update.lock`. `load` (`:142`) reads and parses the whole object on every call.
- Staleness (`store.rs:137`): `refresh.pending` exists, or a fresh `snapshot()` (git `ls-files --cached --others --exclude-standard` then a hash of **every** file) differs. Cost is proportional to repository bytes; not measured.
- Invalidation (`store.rs:306 refresh`): validates paths, writes `refresh.pending`, spawns a detached process group running `update` (a full rebuild). Per-path refresh arguments are only validated, not used to limit work.
- Cache location: `Store::open` runs `git rev-parse --show-toplevel` and `git rev-parse --git-path omp-kb` (`store.rs:104`). In a linked worktree this resolves to `<repo>/.git/worktrees/<name>/omp-kb` (checked: `git rev-parse --git-path omp-kb` in `/private/tmp/omp-gateway-research` printed `/Users/enverfrancisco/repositories/oh-my-pi/.git/worktrees/omp-gateway-research/omp-kb`). Two Git worktrees of one repository therefore build two indexes.
- Doc drift: `crates/kb/README.md` links `docs/knowledge-base/README.md#query-the-code-index`, a heading that file does not have (its headings are "Index tiers", "Artifacts", "Query examples", "Limits (honest)"); `crates/kb/src/main.rs:13` says "Query a revision-aware Rust SCIP index" although three producers exist. `docs/knowledge-base/README.md` tier 2 still describes `scripts/knowledge-base/gen-scip.sh`, a separate path from `omp-kb`.
- Size: `docs/research/grammar-bundling.md:41` records `omp-kb` at 97.3 MB with all grammars and 1.2 MB with none (the research's own measurement; not repeated here).
- Tests: `crates/kb/tests/cli.rs` (`indexes_calls_implementations_and_preserves_last_good_snapshot`, `indexes_impls_for_reference_and_primitive_types`, `indexes_a_repository_without_a_committed_lockfile`, `works_in_a_second_repository`) and `languages.rs` (`failed_producer_keeps_previous_bytes`, `failed_refresh_marks_the_last_good_index_stale`, syntactic fallbacks, committed `typescript.scip` and `python.scip` fixtures). A code comment at `cli.rs:52` reports one SCIP run measured at 58 s blocked in metadata and 84 s in build scripts for about 3 s of analysis; that is a claim in source, not a measurement made here.

## 4. Who calls omp-kb

- `omp kb {update,query,refresh}`: `crates/app/src/kb_cmd.rs:52 run`, wired at `crates/app/src/cli.rs:1231` (`Command::Kb`) and `:3362`.
- The standalone binary `crates/kb/src/main.rs` (same three subcommands).
- Searched: `git grep -n -i -w -E 'omp_kb|omp-kb|omp kb'` over `crates`, `extensions`, `.omp`, `AGENTS.md`, `justfile`, `.github`. No tool, no Director, no hook and no extension calls `Store`. `docs/adr/0060-semantic-find.md:30` independently states "only consumer is `crates/app/src/kb_cmd.rs`". `refresh` is only reachable from the CLI, so nothing schedules invalidation after an edit.
- SCIP outside `crates/kb`: `git grep -n -i -w scip -- crates ':!crates/kb'` returns nothing.

## 5. Revision, document version and invalidation

- kb: `Snapshot { revision: HEAD, fingerprint: hash of {path: hash} }` (`store.rs:20`). Dirty working-tree files are covered by the fingerprint, not by the revision. A query reports `revision`, `fingerprint`, `stale`.
- Environment: `DurableSnapshotRecord` with a `u64` generation, snapshot id, manifest hash, tree hash (`crates/envd/src/workspace/operations.rs:275`); `create_worktree` (`:673`) refuses a base that is not the live generation.
- LSP tool: the module doc says "Revisioned"; its signature uses `Rev` and `DocEffects` from `omp_tool` (`crates/tools/src/lsp/mod.rs:3`, imports). Not read further.
- No single key joins these. An SCIP index built from files at fingerprint F cannot be tied to an envd snapshot id without a new mapping.

## 6. Do subagents share or rebuild the index?

- Children get a clone view of tracked plus untracked non-ignored files with `.git` excluded (`docs/adr/0007-subagent-filesystem-isolation.md`, Decision, first bullet), rooted under the envd state directory (`operations.rs:482`, `:694`), not under the repository.
- `Store::open` needs `git rev-parse --show-toplevel` to succeed. In a directory with no `.git` and no parent repository it exits 128 (`fatal: not a git repository`, checked in a temp directory). Hence `omp kb` cannot index or query inside a child view as built, by inspection and that one check; the real child path was not run.
- Nothing in the subagent spawner references kb. Verdict: children neither share nor rebuild an omp-kb index; the index is not used by agents at all.
- Contrast: Mnemopi banks are keyed by "Canonical primary Git root from the Environment repository snapshot. Linked worktrees must receive the same value" (`crates/memory/src/bank.rs:48`), so memory is shared across worktrees while kb is not.
- LSP: the tool talks to "the project Environment's document authority" (`LspControl`, `mod.rs:353`), so language servers are environment-owned; how many servers a swarm starts was not examined.

## 7. Memory and the ADRs

- Mnemopi (`crates/memory`): SQLite banks, `fts_working`, `fts_episodes`, `fts_facts` FTS5 tables (`store.rs:1718-1750`), `bm25()` ranking (`store.rs:834`, `:1185`), dense cosine voice (`recall.rs:261`), graph voice over triples and links (`link.rs:1`, `MAX_LINKS_PER_MEMORY = 8`), fusion by reciprocal rank with `RRF_K = 60` and voice weights 0.35, 0.25, 0.25, 0.15 (`recall.rs:16`, `:46`). Content is conversation memory, "derived exclusively from authoritative memory content" (`link.rs`, `reconcile` doc). It does not index source code.
- ADR 0062 (`docs/adr/0062-mnemopi-fact-extraction.md`): journal-keyed retention and model-free English and Spanish fact rules; status Proposed, "not owner-approved; not started" (STATUS.md). Everything is behind default-off convars. A worktree `~/.omp/wt/mnemopi-fact-extraction-oh-my-pi` on branch `feat/mnemopi-fact-extraction` (`git worktree list`, head `daab333d13`) exists; its content was not read, label `Unmerged`.
- ADR 0060 (`docs/adr/0060-semantic-find.md`): a `locate` device, a lexical prior then a `judge` model cascade, "no index, no wire slot", T1 only if a spike (D1) passes. `omp-kb` is recorded there as "a symbol graph ... No file content, no embeddings". It is file search, not symbol relations.
- ADR 0053 (`docs/adr/0053-independent-verdicts.md`): acceptance decided from evidence by an independent verdict; D3 defines the `judge` role (as cited in ADR 0060). It bounds semantic judgments: a model verdict is not authority for symbol facts, and `@judge` is for acceptance. STATUS row: Partially implemented.

## 8. Other relevant facts

- `docs/architecture/p1-capability-inventory.md:68`: "Glean / Angle service | absent in both | REMOVE".
- `crates/ast/src/parse_cache.rs:1`: a bounded content-addressed parse cache; its doc comment reports 13.5 ms (81 KB TypeScript) and 187 ms (1.05 MB) per parse on an M4 Max (source-comment claim, not repeated).
- Cargo: `scip = "0.10.0"` (`Cargo.toml:226`) and `protobuf = "3.7.2"` (`Cargo.toml:205`) equal the versions in `scip-code/scip@8fe5037:bindings/rust/Cargo.toml` (`scip` 0.10.0, `protobuf =3.7.2`), which also exposes `write_message_to_file` and `symbol::{parse_symbol, format_symbol}`; `omp-kb` uses neither.
- No Tantivy and no Glean in `Cargo.lock` (`git grep -n -i -E 'tantivy|glean' -- Cargo.lock` empty).

## 9. Not examined

LSP server lifecycle and pooling; `crates/ast/src/language` grammars; `crates/chat`/TUI uses of kb data; the `feat/mnemopi-fact-extraction` branch; `scripts/knowledge-base/gen-scip.sh` internals; any measurement of index build, query or staleness time.

Back to the [index](../README.md).
