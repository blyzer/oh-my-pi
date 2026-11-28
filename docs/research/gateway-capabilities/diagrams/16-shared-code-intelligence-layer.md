# Shared code intelligence layer

> Part of the [OMP2 gateway-capabilities research](../README.md). Diagram for deliverables 11 and 12. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: PROPOSED architecture view. Elements marked EXISTING are verified in code and cited; elements marked PROPOSED do not exist. Authority flows from the resolved layer; semantic and associative layers only nominate candidates.

```mermaid
flowchart TB
  subgraph Agents["Root session and children (EXISTING logical swarm, diagram 13)"]
    root["Root kernel"]
    c1["Child A: CoW view, no .git (EXISTING docs/adr/0007)"]
    c2["Child B: CoW view, no .git"]
  end
  tool["PROPOSED code query tool (CIX-02), result type CodeQueryResult (CIX-11)"]
  key["PROPOSED index key: primary root + envd snapshot id + producer versions (CIX-03)"]
  subgraph Resolved["Resolved layer (authoritative)"]
    kb["omp-kb Index, JSON in git-path omp-kb (EXISTING crates/kb/src/store.rs:268)"]
    lsp["LSP tool via LspControl (EXISTING crates/tools/src/lsp/mod.rs:354)"]
  end
  subgraph Structural["Structural layer"]
    ast["omp-ast parse cache, summaries (EXISTING crates/ast/src/parse_cache.rs:1)"]
    sg["ast_grep and ast_edit (EXISTING crates/tools/src/ast_grep.rs:417)"]
  end
  subgraph Lexical["Lexical layer"]
    grep["omp-grep, glob, walker (EXISTING crates/grep/src/lib.rs)"]
    tri["fuzzy-index trigrams, Python T2 (EXISTING extensions/fuzzy-index)"]
  end
  subgraph Advisory["Semantic and associative layers (advisory only)"]
    mem["Mnemopi four-voice recall, conversations (EXISTING crates/memory/src/recall.rs:34)"]
    loc["ADR 0060 locate (PROPOSED in ADR, not started)"]
  end
  env["envd Environment: snapshots, generation, manifest hash (EXISTING crates/envd/src/workspace/operations.rs:275)"]
  root --> tool
  c1 --> tool
  c2 --> tool
  tool --> key --> kb
  tool --> lsp
  tool --> ast
  tool --> grep
  tool -. "candidates only" .-> mem
  tool -. "candidates only" .-> loc
  key --> env
  kb -->|"SCIP import (EXISTING)"| prod["rust-analyzer, scip-typescript, scip-python (EXISTING producer.rs:55)"]
```

```text
 EXISTING: root and child kernels             PROPOSED: code query tool + CodeQueryResult (CIX-02, CIX-11)
        |                                                   |
        +--------------------+------------------------------+
                             |
                  PROPOSED index key (CIX-03)
        (primary root + envd snapshot id + producer versions)
             |                    |                     |
   RESOLVED (authoritative)   STRUCTURAL            LEXICAL
   omp-kb Index (EXISTING)    omp-ast, ast-grep     omp-grep, fuzzy-index
   LSP via Environment        (EXISTING)            (EXISTING)
   (EXISTING)
             ^
             | SCIP import from rust-analyzer, scip-typescript, scip-python (EXISTING)

   ADVISORY, candidates only, never a relation source:
   Mnemopi recall (EXISTING, conversations)   ADR 0060 locate (PROPOSED, not started)

 Today: only `omp kb` (EXISTING crates/app/src/kb_cmd.rs:52) reaches omp-kb; children cannot (no .git).
```

Reading notes. Dotted edges mean the result is a hint that must be rechecked in the resolved or lexical layer. The index key replaces the Git-path cache location (`store.rs:105`) for children; the parent keeps its current cache. No new daemon is proposed; the query tool runs in the session process and reads the shared index through the Environment.

Back to the [index](../README.md).
