# Reference repository inventory

Cloned 2026-11-28 with `git clone --depth 1 --no-tags https://github.com/<repo>.git` into `/private/tmp/omp-research-refs/<group>/<owner>_<repo>/` (outside the OMP2 tree). Pinned SHA = `git rev-parse HEAD` of the shallow clone; `Head date` = `git log -1 --format=%cI`. Licence column = GitHub API `license.spdx_id` (`-` = none detected) and the licence files found in the clone. Stars/archived/pushed came from `gh api repos/<repo>` on the same day and are context, not evidence about code quality.

Command used for the clones and the table: see `evidence/ref-inventory-commands.md`.

| Group | Repository | Status | Pinned SHA | Head date | Branch | Licence (API / files) | Archived | Stars |
|---|---|---|---|---|---|---|---|---|
| abi | [bytecodealliance/wasmtime](https://github.com/bytecodealliance/wasmtime) | cloned | `b738a89287bb78e51c06ac9624014bbda3c1429f` | 2026-11-25 | main | Apache-2.0 / LICENSE | False | 18905 |
| abi | [bytecodealliance/wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) | cloned | `e3b53be159268c3925a6e0a23eef6419dfa5bbea` | 2026-11-19 | main | Apache-2.0 / LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT | False | 1481 |
| abi | [capnproto/capnproto-rust](https://github.com/capnproto/capnproto-rust) | cloned | `d53310eb22e305b80172889870d7fea3314ed20c` | 2026-11-27 | master | MIT / LICENSE | False | 2523 |
| abi | [djkoloski/rkyv](https://github.com/djkoloski/rkyv) | cloned | `d963abc24301a571605a9bafd963dfed72187fd7` | 2026-09-25 | main | MIT / LICENSE | False | 4408 |
| abi | [google/flatbuffers](https://github.com/google/flatbuffers) | cloned | `5eb190f00005b8dd1aa4f7ab2db9f01a39e3c76d` | 2026-11-06 | master | Apache-2.0 / LICENSE | False | 26666 |
| abi | [hashicorp/go-plugin](https://github.com/hashicorp/go-plugin) | cloned | `415e29b9562b51ba63782ecbd719927b022c43eb` | 2026-11-02 | main | MPL-2.0 / LICENSE | False | 6134 |
| abi | [hyperium/tonic](https://github.com/hyperium/tonic) | cloned | `c7e68c59d40ee667ec09cbe9e056a440fc58550d` | 2026-11-27 | master | MIT / LICENSE | False | 12595 |
| abi | [tokio-rs/prost](https://github.com/tokio-rs/prost) | cloned | `894f4a7f9c41f1afb8455fb6e521c067815f608d` | 2026-11-12 | master | Apache-2.0 / LICENSE | False | 4844 |
| config | `AmethystDev-Labs/QuickCLI` | **unavailable** (`git clone` and `gh api repos/AmethystDev-Labs/QuickCLI` both returned not found/404 on 2026-11-28) | - | - | - | - | - | - |
| config | [Ve-ria/quickx](https://github.com/Ve-ria/quickx) | cloned | `e13db37a6395ea4cb9c2b23315234cdc38aefdc4` | 2026-04-07 | main | GPL-3.0 / LICENSE | False | 0 |
| config | [lzt404/EasyCLIProxyAPI](https://github.com/lzt404/EasyCLIProxyAPI) | cloned | `faf29b221df867f0efcfb91f567442dc62b97321` | 2026-10-01 | main | MIT / LICENSE | False | 0 |
| config | [stark-shadow/ecpa-plugins-store](https://github.com/stark-shadow/ecpa-plugins-store) | cloned | `2d8228927f8d9af93b8700dd1bc06906baa72985` | 2026-11-27 | main | - / none | False | 0 |
| harness | [Ve-ria/cotion](https://github.com/Ve-ria/cotion) | cloned | `b7a43075e2498b6c8b5d93ef2c103e03ed4eed67` | 2026-06-14 | main | MIT / LICENSE | False | 1 |
| harness | [grape-dev/grape](https://github.com/grape-dev/grape) | cloned | `b31f8d8cef94a10baaa8cf0fdb9da26d7cb65501` | 2026-11-26 | main | MIT / LICENSE | False | 1445 |
| harness | [limecloud/lime](https://github.com/limecloud/lime) | cloned | `015b8788bfa5dc22bd23d1df25fc943d48e8b9be` | 2026-11-18 | main | GPL-3.0 / LICENSE, licenses, LICENSES.md | False | 1501 |
| inference | [QuantumNous/new-api](https://github.com/QuantumNous/new-api) | cloned | `04dfcbb7128982d78b63a2ecb15f6d20c7305f44` | 2026-11-28 | main | AGPL-3.0 / LICENSE | False | 52658 |
| inference | [Ve-ria/CLIProxyAPIPlus](https://github.com/Ve-ria/CLIProxyAPIPlus) | cloned | `60bb379e22f6991f9b83505f768a60cbe4f01400` | 2026-05-09 | main | AGPL-3.0 / LICENSE, LICENSE-MIT | False | 70 |
| inference | [cheaper-inference/OmniRoute](https://github.com/cheaper-inference/OmniRoute) | cloned | `3e23a8afe026a4980926c2fcab5b1fcfabb995c4` | 2026-11-27 | release/v3.8.55 | MIT / LICENSE | False | 90792 |
| inference | [decolua/9router](https://github.com/decolua/9router) | cloned | `5ae63d22847e07042e235b40e1d926b65e509013` | 2026-11-23 | master | MIT / LICENSE | False | 32932 |
| inference | [fallencolors/freellmapi](https://github.com/fallencolors/freellmapi) | cloned | `b5d8333177cb7521488b6fa7ed89294058c0c029` | 2026-09-23 | main | MIT / LICENSE | False | 1 |
| inference | [hank9999/proxycast](https://github.com/hank9999/proxycast) | cloned | `3ad8ff64b980b4a69d3d2d1b3dd8bbef1ab2db8b` | 2025-12-26 | main | - / none | False | 8 |
| inference | [nicolaeser/llmhub](https://github.com/nicolaeser/llmhub) | cloned | `8bdc1deb5a2996408c83f86e94c0a4dde3588294` | 2026-11-26 | main | MIT / LICENSE | False | 2 |
| inference | [router-for-me/CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) | cloned | `dbe641f286a489ecb39e3c041c554371e6add864` | 2026-11-27 | main | MIT / LICENSE | False | 59626 |
| local | [apple/coremltools](https://github.com/apple/coremltools) | cloned | `a207b1a3452eafe02f2d870f486287834af2c82d` | 2026-09-30 | main | BSD-3-Clause / LICENSE.txt | False | 5462 |
| local | [facebookincubator/Glean](https://github.com/facebookincubator/Glean) | cloned | `92ba871deeabf3367af74235bcebd97f6d409928` | 2026-11-27 | main | NOASSERTION / LICENSE | False | 1446 |
| local | [ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) | cloned | `8e23278c7c36f4429fb237467d50e2935a91e655` | 2026-11-27 | master | MIT / LICENSE, licenses | False | 136457 |
| local | [huggingface/candle](https://github.com/huggingface/candle) | cloned | `9c1fcffaf956fcfd629fdddc9fc25adefb95c052` | 2026-11-26 | main | Apache-2.0 / LICENSE-APACHE, LICENSE-MIT | False | 21436 |
| local | `microsoft/onnxruntime` | **unavailable** (`git clone` and `gh api repos/microsoft/onnxruntime` both returned not found/404 on 2026-11-28) | - | - | - | - | - | - |
| local | [ml-explore/mlx](https://github.com/ml-explore/mlx) | cloned | `2e094ae588736f5fd73ea084b44b51a16a443526` | 2026-11-28 | main | MIT / LICENSE | False | 29310 |
| local | [ml-explore/mlx-lm](https://github.com/ml-explore/mlx-lm) | cloned | `6fe0ce48c90cec11f2c305830ceba461051c33b2` | 2026-11-24 | main | MIT / LICENSE | False | 7779 |
| local | [quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy) | cloned | `c005b68425675a1a022f5c28c872d932260e454b` | 2026-11-26 | main | MIT / LICENSE | False | 16493 |
| local | [scip-code/scip](https://github.com/scip-code/scip) | cloned | `8fe503757e133d98312b059b89e0ad5f72852d53` | 2026-11-21 | main | Apache-2.0 / LICENSE | False | 907 |
| provider | [BenedictKing/kiro.rs](https://github.com/BenedictKing/kiro.rs) | cloned | `87a735c6b7242d9de41fbd8f595511e8476ed53d` | 2026-07-03 | master | - / none | False | 90 |
| provider | [Foxfishc/kiro.rs](https://github.com/Foxfishc/kiro.rs) | cloned | `15c0a3af4c6c7dc26436bb275f41aa8bb9c73c1f` | 2026-07-16 | master | MIT / LICENSE | False | 37 |
| provider | [M-JYuan/kiro.rs](https://github.com/M-JYuan/kiro.rs) | cloned | `fc3528317bdc527d5c57212d646e9f379aada602` | 2026-06-18 | master | MIT / LICENSE | False | 90 |
| provider | [cxy2730/CLIProxyAPI-freetier2api-plugin](https://github.com/cxy2730/CLIProxyAPI-freetier2api-plugin) | cloned | `0a718a6d463507a7fef886ee22ce61ee5a973f4f` | 2026-11-27 | main | MIT / LICENSE | False | 38 |
| provider | [hank9999/kiro.rs](https://github.com/hank9999/kiro.rs) | cloned | `3c9723fc8968c2aa1c4a5eb131bf2bcd45358117` | 2026-11-25 | master | MIT / LICENSE | False | 2024 |
| provider | [lzt404/kiro.rs](https://github.com/lzt404/kiro.rs) | cloned | `833158518a21f749ff61121b35dd5b16c73e19ad` | 2026-05-29 | master | MIT / LICENSE | False | 0 |
| provider | [stark-shadow/cpa-plugin-perplexity](https://github.com/stark-shadow/cpa-plugin-perplexity) | cloned | `0d0916126fa24eb8a469d87a9b62c5e0ce4221c9` | 2026-11-10 | main | MIT / LICENSE | False | 2 |

Back to the [index](../README.md).
