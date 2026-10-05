# Semantic index evaluation

Question: how many of the tokens the Claude history spent on finding and reading code could a
compiler-backed semantic index (rust-analyzer) have answered more cheaply? The Graphify evaluation
(2026-10-05) found that a tree-sitter code graph does not: its `affected` queries missed most usages
`rg` found. This experiment asks the same questions of rust-analyzer, whose resolution is exact.

No model or provider credentials are involved; it is an offline comparison.

## Inputs (`data/`)

* `usages.json` - 165 usage-style `rg <identifier>` searches from the history (symbol, how often it
  was run, average result tokens, and the repo files `rg` returned). No transcript text.
* `history_stats.json` - aggregate token counts of the history by tool category (from
  `scripts/logs.py` and `scripts/detail.py`).

## Method (`scripts/lsp_eval.py`, run by `.github/workflows/semantic-index-eval.yml`)

* **A. References.** For each search, `workspace/symbol` then `textDocument/references`; compare the
  tokens of the answer (`path:line` list, and grouped by file) with the `rg` result, and how many of
  the Rust files `rg` returned the index also names (recall; `rg` also matches comments and strings,
  so recall is a lower bound). `workspace/symbol` is asked for every symbol kind (`allSymbols`);
  rust-analyzer lists only types by default.
* **B. Symbol slices.** Sample 120 source files, take every function and method body from
  `textDocument/documentSymbol`, and compare their size with the average `sed -n` range read in the
  history (975 tokens) and whole-file `cat` (1 797).

## What the history says is at stake

Bash tool results were 7.4 M tokens: file reading 3.28 M (of which `sed -n` ranges 2.77 M),
code search 1.44 M, git 1.0 M, verification 0.54 M, logs 0.34 M. Only verification and log output
has a low signal share (24-27 %); a typed verdict would trim roughly 0.65 M. The larger lever is
reading by symbol instead of by line range, and references instead of repeated `rg`.

## Results

Filled in from the workflow artifact (`semantic-index-eval-results/summary.txt`).
