# Knowledge base — semantic memory

Durable offline map of **where symbols live** and **which domain owns which semantics**.

| Artifact | Purpose |
| --- | --- |
| [`../knowledge-base.md`](../knowledge-base.md) | Architecture / ADR / crate narrative index |
| [`semantics.md`](semantics.md) | Domain → owning crates → anchor types |
| [`lookup.json`](lookup.json) | Exact `SymbolName` → `[{crate, kind, path, line}]` |
| [`summary.json`](summary.json) | Totals, domain grouping, per-crate counts, `lib.rs` mods |
| [`by-crate/*.jsonl`](by-crate/) | Full module-level `pub` API dump, one crate per file |
| [`../../scripts/knowledge-base/gen-symbols.py`](../../scripts/knowledge-base/gen-symbols.py) | Regenerator |

## Quick use

```sh
# Where is Kernel defined?
python3 -c 'import json; print(json.load(open("docs/knowledge-base/lookup.json"))["Kernel"])'

# Regenerate after large refactors
python3 scripts/knowledge-base/gen-symbols.py
```

## Limits

- Column-0 `pub` scan only (module-level API). Nested/`pub(crate)` items are omitted.
- Not a rustc AST / SCIP index. Name collisions are listed as multiple locations.
- rust-analyzer remains authoritative for navigation inside the IDE.
