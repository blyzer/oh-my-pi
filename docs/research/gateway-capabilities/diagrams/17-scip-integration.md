# SCIP integration

> Part of the [OMP2 gateway-capabilities research](../README.md). Diagram for deliverables 11 and 12. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: PROPOSED architecture view for the export and sharing parts; the import path is EXISTING and cited. Export is shown as deferred (CIX-04), not as planned work.

```mermaid
flowchart LR
  subgraph Producers["External producers (EXISTING crates/kb/src/producer.rs:55)"]
    ra["rust-analyzer scip"]
    ts["scip-typescript index"]
    py["scip-python index"]
    more["PROPOSED: other SCIP indexers as table rows"]
  end
  raw["producer .scip temp file (EXISTING store.rs:186)"]
  read["Index::read_scip (EXISTING graph.rs:408)"]
  guard["snapshot before/after, HEAD and file checks (EXISTING store.rs:168, error.rs:31)"]
  decode["Index::decode: defs, refs, relationships (EXISTING graph.rs:423)"]
  syn["tree-sitter calls and syntactic fallback (EXISTING graph/syntax.rs:174)"]
  obj["objects/<digest>.json + index.json head (EXISTING store.rs:268)"]
  q["omp kb query (EXISTING kb_cmd.rs:62)"]
  keep["PROPOSED, deferred (CIX-04): keep producer bytes beside the object"]
  ext["External consumers: scip lint/print/stats, Glean, Sourcegraph (not integrated)"]
  ra --> raw
  ts --> raw
  py --> raw
  more --> raw
  raw --> read --> guard --> decode --> syn --> obj --> q
  raw -.-> keep -.-> ext
```

```text
 producers (EXISTING)                      OMP2 import (EXISTING, crates/kb)
 rust-analyzer scip   \
 scip-typescript index  >--> temp .scip --> read_scip --> snapshot guard --> decode
 scip-python index    /      (store.rs:186)  (graph.rs:408)  (store.rs:168)   (graph.rs:423)
 PROPOSED: more indexers                                                         |
        (producer.rs table rows)                                   + tree-sitter calls, syntactic fallback
                                                                                 v
                                                  objects/<digest>.json + index.json  --> omp kb query
        .scip bytes
          : (PROPOSED, deferred CIX-04) keep unmodified beside the object
          v
   external consumers: scip lint/print/stats, Glean, Sourcegraph (not integrated)
```

Attach points, all in `crates/kb`: producer table rows; unused SCIP fields (roles other than Definition, `is_type_definition`, `enclosing_range`, `Diagnostic`, `documentation`) that `graph.rs` could read; per-document reuse (CIX-06); coverage report (CIX-14). SCIP carries no revision field (`scip.proto:41`), so the revision and fingerprint binding in `store.rs:20` stays the consumer's responsibility. No re-encoding exporter is proposed (CIX-04).

Back to the [index](../README.md).
