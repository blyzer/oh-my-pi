# Knowledge base — semantic memory

Durable offline map of **where symbols live** and **which domain owns which semantics**.

## Index tiers (how we close the AST gap)

| Tier | Command | What you get | What you don't |
| --- | --- | --- | --- |
| **0 · Text** | `python3 scripts/knowledge-base/gen-symbols.py` | Fast column-0 `pub` API surface → [`lookup.json`](lookup.json) | Nested/`pub(crate)`/impl methods |
| **1 · AST outline** (default durable) | `python3 scripts/knowledge-base/gen-ast-symbols.py` | Per-file rust-analyzer structure: structs, traits, **impl methods**, private items, qualified names → [`ast-lookup.json.gz`](ast-lookup.json.gz) | Cross-file references, trait-impl resolution across crates |
| **2 · SCIP** | `scripts/knowledge-base/gen-scip.sh` | Workspace semantic graph (defs + refs + external symbols) → `index.scip` | Slow; needs clean analysis (this repo's Cranelift/pyo3 graph can fail hard) |
| **3 · Live LSP** | IDE / agent `lsp` tools | Exact definition/refs/impls right now | Not a durable checked-in artifact |

**Solve the “not a full AST” limit by using Tier 1 day-to-day**, regenerate after large refactors, and reach for Tier 2/3 when you need references or cross-crate impls.

## Artifacts

| Artifact | Purpose |
| --- | --- |
| [`../knowledge-base.md`](../knowledge-base.md) | Architecture / ADR / crate narrative index |
| [`semantics.md`](semantics.md) | Domain → owning crates → anchor types |
| [`ast-lookup.json.gz`](ast-lookup.json.gz) | **AST durable memory** (name / `Type::method` → path:line) |
| [`ast-summary.json`](ast-summary.json) | AST index stats by kind/crate |
| [`lookup.json`](lookup.json) | Tier-0 pub text scan (lighter, incomplete) |
| [`summary.json`](summary.json) | Tier-0 stats |
| [`by-crate/*.jsonl`](by-crate/) | Tier-0 per-crate dumps |
| [`../../scripts/knowledge-base/`](../../scripts/knowledge-base/) | Regenerators |

`index.scip`, uncompressed `ast-lookup.json`, and `by-crate-ast/` are gitignored local rebuild products.

## Query examples

```sh
# AST memory: where is Kernel::run_turn?
python3 - <<'PY'
import gzip, json
idx = json.loads(gzip.open("docs/knowledge-base/ast-lookup.json.gz"))
print(idx["Kernel::run_turn"])
print(idx["compose_kernel"])
PY

# Regenerate AST index (~15–20s)
python3 scripts/knowledge-base/gen-ast-symbols.py

# Full outline including fields/variants (local only)
OMP_KB_AST_FULL=1 OMP_KB_AST_KEEP_JSONL=1 python3 scripts/knowledge-base/gen-ast-symbols.py

# Heavy SCIP (optional; may take tens of minutes / fail on unclean analysis)
scripts/knowledge-base/gen-scip.sh
```

## Limits (honest)

- Tier 1 uses rust-analyzer's **per-file** `symbols` outline — parent-qualified paths like `Kernel::run_turn` work; generics are stripped for lookup aliases (`Kernel<C>::run_turn` also answers to `Kernel::run_turn`).
- Tier 1 is **not** type-aware: it will not tell you every `impl Trait for T` across the crate graph. That is Tier 2 (SCIP) or Tier 3 (LSP).
- Name collisions remain multi-hit lists; disambiguate with `crate` / `qual` / `path`.
