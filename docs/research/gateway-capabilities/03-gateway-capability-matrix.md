# Cross-gateway capability matrix

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 3. Reference repositories pinned in [`evidence/ref-inventory.md`](evidence/ref-inventory.md). Research proposal: nothing here is implemented.

**Status of this document: first pass.** It records what each reference repository is, its pinned commit, its licence and how much code it has, all of which the parent verified directly. The capability-by-capability matrix (dimension IDs G, A, S, T, I, E, X, O against each gateway) is assembled mechanically from the per-repository files `evidence/matrix-<owner>_<repo>.tsv`; those are written by area agents and were not yet complete when this commit was made, so that section is marked pending and **no capability claim about a reference repository is made here**.

## 1. Availability

Of the 39 repositories named in the owner's section 3, 37 were cloned shallowly (`git clone --depth 1 --no-tags`) and 2 were unavailable on 2026-11-28:

- `AmethystDev-Labs/QuickCLI`: `git ls-remote` printed `ERROR: Repository not found`; `gh api repos/AmethystDev-Labs/QuickCLI` returned HTTP 404; `gh api users/AmethystDev-Labs/repos` lists no repository with that name. Not assessed.
- `microsoft/onnxruntime`: same two errors. The sibling repositories `microsoft/onnxruntime-genai`, `-extensions` and `-inference-examples` also returned not found, while `microsoft/vscode` and `pykeio/ort` resolved. ONNX Runtime can therefore only be assessed from its official documentation (labelled "documentation only, code not read") in deliverable 10.

## 2. What each cloned repository is (verified facts only)

Source-file counts are the number of files with a source extension (`.go .rs .ts .tsx .js .py .java .cpp .cc .c .swift .kt .vue .mjs`) found by walking the clone, skipping `.git`, `node_modules`, `vendor` and `target`; the three most common extensions are shown. Licence is the GitHub API `license.spdx_id` plus the licence files present in the clone. The README title is the first Markdown heading. None of this says what the code does; that is the reading in `evidence/ref-*.md`.


### Inference gateways

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [QuantumNous/new-api](https://github.com/QuantumNous/new-api) | `04dfcbb` | AGPL-3.0 / LICENSE | .go 1152, .tsx 983, .ts 527 | New API |
| [Ve-ria/CLIProxyAPIPlus](https://github.com/Ve-ria/CLIProxyAPIPlus) | `60bb379` | AGPL-3.0 / LICENSE, LICENSE-MIT | .go 617, .tsx 40, .ts 4 | CI/CD |
| [cheaper-inference/OmniRoute](https://github.com/cheaper-inference/OmniRoute) | `3e23a8a` | MIT / LICENSE | .ts 15352, .tsx 1955, .mjs 951 | OmniRoute - The Free AI Gateway |
| [decolua/9router](https://github.com/decolua/9router) | `5ae63d2` | MIT / LICENSE | .js 1610, .mjs 17 | 9Router - FREE AI Router & Token Saver |
| [fallencolors/freellmapi](https://github.com/fallencolors/freellmapi) | `b5d8333` | MIT / LICENSE | .ts 612, .tsx 104, .mjs 11 | FreeLLMAPI |
| [hank9999/proxycast](https://github.com/hank9999/proxycast) | `3ad8ff6` | none detected / no licence file | .rs 203, .tsx 136, .ts 65 | ProxyCast 🚀 |
| [nicolaeser/llmhub](https://github.com/nicolaeser/llmhub) | `8bdc1de` | MIT / LICENSE | .ts 1083, .tsx 321, .mjs 54 | LLM Hub |
| [router-for-me/CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) | `dbe641f` | MIT / LICENSE | .go 2523, .c 16, .rs 15 | CLI Proxy API |

### Provider implementations

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [BenedictKing/kiro.rs](https://github.com/BenedictKing/kiro.rs) | `87a735c` | none detected / no licence file | .rs 71, .tsx 24, .ts 8 | kiro-rs |
| [Foxfishc/kiro.rs](https://github.com/Foxfishc/kiro.rs) | `15c0a3a` | MIT / LICENSE | .rs 73, .tsx 24, .ts 8 | kiro-rs |
| [M-JYuan/kiro.rs](https://github.com/M-JYuan/kiro.rs) | `fc35283` | MIT / LICENSE | .rs 70, .tsx 20, .ts 8 | kiro-rs |
| [cxy2730/CLIProxyAPI-freetier2api-plugin](https://github.com/cxy2730/CLIProxyAPI-freetier2api-plugin) | `0a718a6` | MIT / LICENSE | .go 299 | FreeTier 2API Plugin |
| [hank9999/kiro.rs](https://github.com/hank9999/kiro.rs) | `3c9723f` | MIT / LICENSE | .rs 53, .tsx 19, .ts 6 | kiro-rs |
| [lzt404/kiro.rs](https://github.com/lzt404/kiro.rs) | `8331585` | MIT / LICENSE | .rs 71, .tsx 23, .ts 8 | kiro-rs |
| [stark-shadow/cpa-plugin-perplexity](https://github.com/stark-shadow/cpa-plugin-perplexity) | `0d09161` | MIT / LICENSE | .go 15, .js 4, .py 2 | Perplexity - a CLIProxyAPI native provider plugin |

### Configuration and extension management

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [Ve-ria/quickx](https://github.com/Ve-ria/quickx) | `e13db37` | GPL-3.0 / LICENSE | .tsx 25, .ts 21, .js 1 | quickx |
| [lzt404/EasyCLIProxyAPI](https://github.com/lzt404/EasyCLIProxyAPI) | `faf29b2` | MIT / LICENSE | .ts 102, .rs 73, .tsx 49 | Overview |
| [stark-shadow/ecpa-plugins-store](https://github.com/stark-shadow/ecpa-plugins-store) | `2d82289` | none detected / no licence file | n/a | ECPA Plugins Store |

### Agent harness and orchestration

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [Ve-ria/cotion](https://github.com/Ve-ria/cotion) | `b7a4307` | MIT / LICENSE | .ts 21, .mjs 3, .tsx 1 | none |
| [grape-dev/grape](https://github.com/grape-dev/grape) | `b31f8d8` | MIT / LICENSE | .py 2520, .js 210, .vue 48 | 1. What Grape is |
| [limecloud/lime](https://github.com/limecloud/lime) | `015b878` | GPL-3.0 / LICENSE, licenses, LICENSES.md | .rs 4403, .ts 3445, .tsx 1339 | Lime |

### ABI-independent extension mechanisms

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [bytecodealliance/wasmtime](https://github.com/bytecodealliance/wasmtime) | `b738a89` | Apache-2.0 / LICENSE | .rs 2153, .cc 60, .c 28 | Installation |
| [bytecodealliance/wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) | `e3b53be` | Apache-2.0 / LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT | .rs 289, .cpp 67, .c 60 | About |
| [capnproto/capnproto-rust](https://github.com/capnproto/capnproto-rust) | `d53310e` | MIT / LICENSE | .rs 156 | Cap'n Proto for Rust |
| [djkoloski/rkyv](https://github.com/djkoloski/rkyv) | `d963abc` | MIT / LICENSE | .rs 169 | Resources |
| [google/flatbuffers](https://github.com/google/flatbuffers) | `5eb190f` | Apache-2.0 / LICENSE | .ts 198, .rs 169, .py 129 | Quick Start |
| [hashicorp/go-plugin](https://github.com/hashicorp/go-plugin) | `415e29b` | MPL-2.0 / LICENSE | .go 81, .py 7 | Go Plugin System over RPC |
| [hyperium/tonic](https://github.com/hyperium/tonic) | `c7e68c5` | MIT / LICENSE | .rs 510, .cc 2, .py 1 | Overview |
| [tokio-rs/prost](https://github.com/tokio-rs/prost) | `894f4a7` | Apache-2.0 / LICENSE | .rs 107 | *PROST!* |

### Local inference and code intelligence

| Repository | Pinned SHA | Licence (API / files) | Source files (top 3 extensions) | README title |
|---|---|---|---|---|
| [apple/coremltools](https://github.com/apple/coremltools) | `a207b1a` | BSD-3-Clause / LICENSE.txt | .py 800, .cc 314, .java 253 | Install |
| [facebookincubator/Glean](https://github.com/facebookincubator/Glean) | `92ba871` | NOASSERTION / LICENSE | .cpp 283, .py 92, .java 56 | Glean |
| [ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) | `8e23278` | MIT / LICENSE, licenses | .cpp 804, .ts 742, .py 272 | llama.cpp |
| [huggingface/candle](https://github.com/huggingface/candle) | `9c1fcff` | Apache-2.0 / LICENSE-APACHE, LICENSE-MIT | .rs 635, .py 35, .js 14 | candle |
| [ml-explore/mlx](https://github.com/ml-explore/mlx) | `2e094ae` | MIT / LICENSE | .cpp 243, .py 135, .js 2 | MLX |
| [ml-explore/mlx-lm](https://github.com/ml-explore/mlx-lm) | `6fe0ce4` | MIT / LICENSE | .py 736, .js 2 | MLX LM |
| [quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy) | `c005b68` | MIT / LICENSE | .rs 529, .py 1 | Fast full-text search engine library written in Rust |
| [scip-code/scip](https://github.com/scip-code/scip) | `8fe5037` | Apache-2.0 / LICENSE | .go 55, .java 37, .kt 15 | SCIP Code Intelligence Protocol |

## 3. Licence consequences for this research

- **AGPL-3.0**: `QuantumNous/new-api`, `Ve-ria/CLIProxyAPIPlus` (the clone also has `LICENSE-MIT`; which files fall under which is not verified). Copyleft over network use: ideas only, no code reuse, no linking, no vendoring.
- **GPL-3.0**: `Ve-ria/quickx`, `limecloud/lime`. Same: study only.
- **No licence detected**: `stark-shadow/ecpa-plugins-store`, `hank9999/proxycast`, `BenedictKing/kiro.rs`. Without a licence the default is "all rights reserved", so nothing may be copied.
- **`facebookincubator/Glean`**: GitHub reports `NOASSERTION`; the licence text itself must be read from the `LICENSE` file before any statement (done in deliverable 11, not here).
- **MIT, Apache-2.0, BSD-3-Clause, MPL-2.0** (`hashicorp/go-plugin` is MPL-2.0, file-level copyleft): permissive or weak copyleft; reuse of a library still goes through `cargo deny` (`deny.toml`) and `THIRD-PARTY-NOTICES.txt`. Not verified against `deny.toml` here.

Deliverable 14 holds the full security and licensing assessment.

## 4. Capability matrix (pending)

To be generated from `evidence/matrix-*.tsv` with the dimension IDs in [`evidence/01-method-and-dimension-ids.md`](evidence/01-method-and-dimension-ids.md). Cell values will be `Y` (implemented, code read), `P` (partial), `N` (searched and absent), `D` (documented only), `U` (not examined), each with an `owner/repo@sha7:path` citation. Until then there is **no** cross-gateway capability comparison in this set.

Back to the [index](README.md).
