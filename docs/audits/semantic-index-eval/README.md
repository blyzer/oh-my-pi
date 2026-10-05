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

Workflow run 37303224940 (rust-analyzer 1.99.0-nightly 2026-08-07, workspace load 118 s, peak RSS 9.8 GB),
artifact `semantic-index-eval-results` (`summary.txt`, `references.json`).

**A. References.** Of 144 usage-style searches that name at least one Rust file, 68 resolve to a
symbol rust-analyzer can answer (44 to a single unique symbol); the rest are text, flags, keys or
symbols that no longer exist at this commit.

| | rg (history) | rust-analyzer |
|---|---|---|
| tokens, median | 294 | 73 as `path:line`, 33 grouped by file |
| tokens summed over the recorded calls | 41 964 | 5 111 grouped by file (-88 %) |
| recall of the Rust files rg returned | | median 0.65; >= 0.5 in 39/68; 0 in 10/68 (unique symbols: median 0.45) |
| query time | | median 0.18 s |

Recall is a lower bound (rg also matches comments and strings). Precision (median 0.67) is not
meaningful: the recorded rg searches were often scoped to one crate directory.

**B. Symbol slices.** 3 191 functions and methods in 120 sampled files: body median 92 tokens, mean
161, p90 340. The history's `sed -n` range reads average 975 tokens and a whole file has a median of
3 144.

**What it is worth.** Usage searches are small in the history (about 109 k of 5.5 M exploration
tokens), so even -88 % there is about 0.1 M tokens. The lever is reading by symbol: 2 842 `sed -n`
reads cost 2.77 M tokens. If half of them could be replaced by a symbol body of 160-340 tokens,
the saving is roughly 1.0-1.2 M tokens, about 20 % of the exploration tokens and a few percent of
all cache-read volume once carry is counted. This is an estimate: a range read can span several
items or non-function code, which a symbol slice does not replace.

Compared with the Graphify evaluation (tree-sitter graph: `affected` found at least half of the
files rg found in 8 of 48 cases), the compiler-backed index is far more accurate, but its value is
in symbol slices, not references.
