# 0079. A shared code index is keyed by workspace identity and revision, and queries return one typed result

Status: proposed (draft written by the gateway-capabilities research; not owner-reviewed, not approved)
Date: 2026-11-28
Area: tools

Read against `omp2` at `85d219ae7f`. Number 0079 is provisional: it is the next free number on that commit and may be taken by another record; the owner may renumber.

**Review status.** Draft. One agent read the code and wrote this from the capability cards named below in [`docs/research/gateway-capabilities/`](../research/gateway-capabilities/README.md). No machine review, no owner review, nothing measured. It changes no existing ADR; where it would amend one, that is a proposal for the owner to apply.

## Context

`omp-kb` builds a revision-aware index from external SCIP producers and tree-sitter (`crates/kb/Cargo.toml` description; producers in `crates/kb/src/producer.rs`; `Index::read_scip` at `crates/kb/src/graph.rs:408`); it refuses to build when HEAD or a file moves while producers run (`crates/kb/src/error.rs:31`). The code-intelligence audit ([`evidence/omp2-code-intelligence.md`](../research/gateway-capabilities/evidence/omp2-code-intelligence.md)) found: the cache lives at `git rev-parse --git-path omp-kb` (`crates/kb/src/store.rs:105`), so each linked Git worktree builds its own index; child views exclude `.git` (ADR 0007) and `git rev-parse --show-toplevel` fails there, so a child cannot open the index; only `omp kb` and the `omp-kb` binary call it, no tool or Director does; the query shape (`crates/kb/src/graph.rs:115` `Query`) is untyped to callers; provenance is marked per symbol (`Resolution::Syntactic`) but not per relation. The owner's `CodeQueryIR`, `CodeIR`, `ChangeIR` and `VerificationIR` are not in code; they appear only in `docs/parked/2026-10-06-p14-p16-architecture-notes.md` as parked.

## Decision

1. **The index is named independently of Git**: (canonical primary root, base snapshot id and manifest hash from envd, producer name and version). Children open the parent's index read-only through the Environment; files a child changed form an overlay labelled `syntactic` until the parent reindexes after merge (CIX-03).
2. **One typed `CodeQueryResult`** (workspace identity, revision, fingerprint, per-relation origin and staleness) serialised by the CLI, a future read-only agent tool and children; it has no field for model-derived relations (CIX-11). It replaces, rather than adds to, the four parked IRs.
3. **Provenance is a contract on every relation**: authoritative (from a SCIP producer), syntactic (tree-sitter) or associative (advisory), and semantic or associative results never carry authority (CIX-05, CIX-10).
4. A read-only query surface for agents is added only on top of (1) and (2) (CIX-02).

## Consequences

Children share one index without needing `.git`; stale bases are refused or flagged as `RevisionChangedDuringProducers` is today; authoritative symbol relationships never come from embeddings, text matching or an LLM. No SCIP exporter, Glean or Tantivy is introduced (CIX-04, CIX-08, CIX-09).

## Alternatives considered

Each child runs `omp kb update` (needs `.git`, repeats minutes of producer work). Copying `.git` into every view (excluded by ADR 0007). One global index server (a second control plane, ADR 0006). Building the four IRs as new layers (CIX-12).

## Acceptance (from the cards)

Two children created at snapshot S receive the same index digest; a child edit never changes the parent's index; a query in a child names its base snapshot, overlay file count and whether any overlay file is `syntactic`; one crate-owned type is serialised by the CLI and a tool with golden tests.

## Owner questions

O1. Do the four IR names mean types you intend to build (OQ-17)? O2. Approve a read-only agent query surface.

## References

Cards CIX-02, CIX-03, CIX-05, CIX-10, CIX-11, CIX-12 in [deliverable 11](../research/gateway-capabilities/11-code-intelligence-and-scip-assessment.md); ADR 0007; ADR 0049; ADR 0060.
