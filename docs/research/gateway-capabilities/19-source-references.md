# Source references with pinned commits and relevant paths

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 19. Research proposal: nothing here is implemented.

## 1. OMP2

| Item | Value |
|---|---|
| Repository | `blyzer/oh-my-pi`, branch `omp2` (`git@github.com:blyzer/oh-my-pi.git`) |
| Audited commit | `85d219ae7f` (merge of PR #763, 2026-11-27) = `origin/omp2` after `git fetch origin omp2` on 2026-11-28 |
| Research branch | `docs/research-gateway-capabilities` in worktree `/private/tmp/omp-gateway-research` |
| Governing documents | `AGENTS.md` (Architecture; Locked Deviations from pi, lines 526-582), `docs/adr/STATUS.md`, `docs/adr/TASKS.md`, `docs/adr/0018-provider-is-more-than-stream.md`, `docs/adr/0036-python-for-extensions.md`, `docs/adr/0049-tiered-integration-architecture.md` |
| Paths most cited | `crates/ai/src/layer/stack.rs`, `crates/ai/src/router.rs`, `crates/ai/src/registry.rs`, `crates/ai/src/codec/`, `crates/ai/src/transport/`, `crates/ai/src/auth/`, `crates/catalog/src/`, `crates/proto/proto/omp/`, `crates/serve/src/`, `crates/ext/src/`, `crates/envd/src/exthost/`, `crates/driver/src/subagent/`, `crates/adw/src/`, `crates/kb/src/`, `crates/memory/src/embedding/` |

Citation forms used in this set: OMP2 claims as `path:line` plus symbol; reference claims as `owner/repo@sha7:path:symbol`, where `sha7` is the first seven characters of the pinned SHA below. The mechanical checker `evidence/check_docs.py` confirms that cited files and lines exist and that cited symbols occur in the file; it does not confirm that a claim is true.

## 2. Reference repositories

Cloned 2026-11-28 with `git clone --depth 1 --no-tags` into `/private/tmp/omp-research-refs/<group>/<owner>_<repo>/`. `Top-level paths` are the directories present at the pinned commit (a listing, not a statement about relevance); the per-repository reading notes in `evidence/ref-*.md` name the paths actually read.


### Inference gateways

| Repository | Pinned commit | Date | Licence | Top-level paths |
|---|---|---|---|---|
| [QuantumNous/new-api](https://github.com/QuantumNous/new-api) | `04dfcbb7128982d78b63a2ecb15f6d20c7305f44` | 2026-11-28 | AGPL-3.0 | `.agents`, `.github`, `assets`, `bin`, `common`, `constant`, `controller`, `docs`, `dto`, `e2e`, `electron`, `i18n`, `logger`, `middleware` ... |
| [Ve-ria/CLIProxyAPIPlus](https://github.com/Ve-ria/CLIProxyAPIPlus) | `60bb379e22f6991f9b83505f768a60cbe4f01400` | 2026-05-09 | AGPL-3.0 | `.github`, `assets`, `auths`, `cmd`, `docs`, `examples`, `internal`, `sdk`, `test`, `web` |
| [cheaper-inference/OmniRoute](https://github.com/cheaper-inference/OmniRoute) | `3e23a8afe026a4980926c2fcab5b1fcfabb995c4` | 2026-11-27 | MIT | `.github`, `.husky`, `.vale`, `.vscode`, `@omniroute`, `bin`, `changelog.d`, `config`, `contrib`, `deploy`, `docker`, `docs`, `electron`, `examples` ... |
| [decolua/9router](https://github.com/decolua/9router) | `5ae63d22847e07042e235b40e1d926b65e509013` | 2026-11-23 | MIT | `.github`, `.vscode`, `cli`, `docs`, `gitbook`, `i18n`, `images`, `open-sse`, `public`, `scripts`, `skills`, `src`, `tests` |
| [fallencolors/freellmapi](https://github.com/fallencolors/freellmapi) | `b5d8333177cb7521488b6fa7ed89294058c0c029` | 2026-09-23 | MIT | `.claude`, `.github`, `cli`, `client`, `desktop`, `docker`, `docs`, `examples`, `repo-assets`, `scripts`, `server`, `shared` |
| [hank9999/proxycast](https://github.com/hank9999/proxycast) | `3ad8ff64b980b4a69d3d2d1b3dd8bbef1ab2db8b` | 2025-12-26 | none detected | `.claude`, `.github`, `.husky`, `docs`, `public`, `scripts`, `src`, `src-tauri` |
| [nicolaeser/llmhub](https://github.com/nicolaeser/llmhub) | `8bdc1deb5a2996408c83f86e94c0a4dde3588294` | 2026-11-26 | MIT | `.ai`, `.github`, `lang`, `prisma`, `public`, `scripts`, `src` |
| [router-for-me/CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) | `dbe641f286a489ecb39e3c041c554371e6add864` | 2026-11-27 | MIT | `.github`, `assets`, `auths`, `cmd`, `docs`, `examples`, `internal`, `sdk`, `test` |

### Provider implementations

| Repository | Pinned commit | Date | Licence | Top-level paths |
|---|---|---|---|---|
| [BenedictKing/kiro.rs](https://github.com/BenedictKing/kiro.rs) | `87a735c6b7242d9de41fbd8f595511e8476ed53d` | 2026-07-03 | none detected | `.claude`, `.github`, `.serena`, `admin-ui`, `docs`, `examples`, `src`, `tools` |
| [Foxfishc/kiro.rs](https://github.com/Foxfishc/kiro.rs) | `15c0a3af4c6c7dc26436bb275f41aa8bb9c73c1f` | 2026-07-16 | MIT | `.claude`, `.github`, `admin-ui`, `docs`, `examples`, `src`, `tools` |
| [M-JYuan/kiro.rs](https://github.com/M-JYuan/kiro.rs) | `fc3528317bdc527d5c57212d646e9f379aada602` | 2026-06-18 | MIT | `.claude`, `.github`, `admin-ui`, `docs`, `examples`, `src`, `tools` |
| [cxy2730/CLIProxyAPI-freetier2api-plugin](https://github.com/cxy2730/CLIProxyAPI-freetier2api-plugin) | `0a718a6d463507a7fef886ee22ce61ee5a973f4f` | 2026-11-27 | MIT | `.github`, `cpasdk`, `internal`, `scripts`, `tests` |
| [hank9999/kiro.rs](https://github.com/hank9999/kiro.rs) | `3c9723fc8968c2aa1c4a5eb131bf2bcd45358117` | 2026-11-25 | MIT | `.github`, `admin-ui`, `src`, `tools` |
| [lzt404/kiro.rs](https://github.com/lzt404/kiro.rs) | `833158518a21f749ff61121b35dd5b16c73e19ad` | 2026-05-29 | MIT | `.claude`, `.github`, `admin-ui`, `docs`, `examples`, `src`, `tools` |
| [stark-shadow/cpa-plugin-perplexity](https://github.com/stark-shadow/cpa-plugin-perplexity) | `0d0916126fa24eb8a469d87a9b62c5e0ce4221c9` | 2026-11-10 | MIT | `.github`, `acceptance`, `assets`, `extension` |

### Configuration and extension management

| Repository | Pinned commit | Date | Licence | Top-level paths |
|---|---|---|---|---|
| [Ve-ria/quickx](https://github.com/Ve-ria/quickx) | `e13db37a6395ea4cb9c2b23315234cdc38aefdc4` | 2026-04-07 | GPL-3.0 | `.github`, `src` |
| [lzt404/EasyCLIProxyAPI](https://github.com/lzt404/EasyCLIProxyAPI) | `faf29b221df867f0efcfb91f567442dc62b97321` | 2026-10-01 | MIT | `.github`, `assets`, `docs`, `scripts`, `src`, `src-tauri`, `tests` |
| [stark-shadow/ecpa-plugins-store](https://github.com/stark-shadow/ecpa-plugins-store) | `2d8228927f8d9af93b8700dd1bc06906baa72985` | 2026-11-27 | none detected | none |

### Agent harness and orchestration

| Repository | Pinned commit | Date | Licence | Top-level paths |
|---|---|---|---|---|
| [Ve-ria/cotion](https://github.com/Ve-ria/cotion) | `b7a43075e2498b6c8b5d93ef2c103e03ed4eed67` | 2026-06-14 | MIT | `.cotion`, `src`, `tests`, `tmp` |
| [grape-dev/grape](https://github.com/grape-dev/grape) | `b31f8d8cef94a10baaa8cf0fdb9da26d7cb65501` | 2026-11-26 | MIT | `.github`, `.grape`, `apps`, `assets`, `configs`, `contracts`, `data`, `deploy`, `docs`, `examples`, `help`, `packaging`, `scripts`, `src` ... |
| [limecloud/lime](https://github.com/limecloud/lime) | `015b8788bfa5dc22bd23d1df25fc943d48e8b9be` | 2026-11-18 | GPL-3.0 | `.cargo`, `.codex`, `.github`, `.husky`, `assets`, `docs`, `electron`, `extensions`, `governance`, `homebrew`, `internal`, `licenses`, `lime-rs`, `packages` ... |

### ABI-independent extension mechanisms

| Repository | Pinned commit | Date | Licence | Top-level paths |
|---|---|---|---|---|
| [bytecodealliance/wasmtime](https://github.com/bytecodealliance/wasmtime) | `b738a89287bb78e51c06ac9624014bbda3c1429f` | 2026-11-25 | Apache-2.0 | `.agents`, `.claude`, `.github`, `.oss-scanner`, `benches`, `ci`, `cranelift`, `crates`, `docs`, `examples`, `fuzz`, `pulley`, `scripts`, `src` ... |
| [bytecodealliance/wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) | `e3b53be159268c3925a6e0a23eef6419dfa5bbea` | 2026-11-19 | Apache-2.0 | `.devcontainer`, `.github`, `ci`, `crates`, `src`, `tests` |
| [capnproto/capnproto-rust](https://github.com/capnproto/capnproto-rust) | `d53310eb22e305b80172889870d7fea3314ed20c` | 2026-11-27 | MIT | `.github`, `async-byte-channel`, `benchmark`, `blog`, `capnp`, `capnp-futures`, `capnp-rpc`, `capnpc`, `example` |
| [djkoloski/rkyv](https://github.com/djkoloski/rkyv) | `d963abc24301a571605a9bafd963dfed72187fd7` | 2026-09-25 | MIT | `.cargo`, `.github`, `.vscode`, `benchlib`, `book_src`, `media`, `rkyv`, `rkyv_derive`, `rkyv_dyn`, `rkyv_dyn_derive`, `rkyv_dyn_test` |
| [google/flatbuffers](https://github.com/google/flatbuffers) | `5eb190f00005b8dd1aa4f7ab2db9f01a39e3c76d` | 2026-11-06 | Apache-2.0 | `.bazelci`, `.github`, `CMake`, `android`, `bazel`, `benchmarks`, `dart`, `docs`, `examples`, `go`, `goldens`, `grpc`, `include`, `java` ... |
| [hashicorp/go-plugin](https://github.com/hashicorp/go-plugin) | `415e29b9562b51ba63782ecbd719927b022c43eb` | 2026-11-02 | MPL-2.0 | `.github`, `docs`, `examples`, `internal`, `runner`, `test` |
| [hyperium/tonic](https://github.com/hyperium/tonic) | `c7e68c59d40ee667ec09cbe9e056a440fc58550d` | 2026-11-27 | MIT | `.gemini`, `.github`, `codegen`, `examples`, `grpc`, `grpc-benchmark`, `grpc-google-cloud-auth`, `grpc-protobuf`, `grpc-protobuf-build`, `grpc-rls`, `grpc-rustls`, `grpc-tokio`, `grpc-xds`, `interop` ... |
| [tokio-rs/prost](https://github.com/tokio-rs/prost) | `894f4a7f9c41f1afb8455fb6e521c067815f608d` | 2026-11-12 | Apache-2.0 | `.devcontainer`, `.github`, `benchmarks`, `conformance`, `fuzz`, `prost`, `prost-build`, `prost-derive`, `prost-types`, `protobuf`, `tests`, `tests-2015`, `tests-2018`, `tests-2024` ... |

### Local inference and code intelligence

| Repository | Pinned commit | Date | Licence | Top-level paths |
|---|---|---|---|---|
| [apple/coremltools](https://github.com/apple/coremltools) | `a207b1a3452eafe02f2d870f486287834af2c82d` | 2026-09-30 | BSD-3-Clause | `.github`, `cmake`, `coremlpython`, `coremltools`, `deps`, `docker`, `docs`, `docs-guides`, `examples`, `milstoragepython`, `mlmodel`, `modelpackage`, `reqs`, `scripts` |
| [facebookincubator/Glean](https://github.com/facebookincubator/Glean) | `92ba871deeabf3367af74235bcebd97f6d409928` | 2026-11-27 | NOASSERTION | `.github`, `glean`, `mk`, `thrift` |
| [ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) | `8e23278c7c36f4429fb237467d50e2935a91e655` | 2026-11-27 | MIT | `.devops`, `.gemini`, `.github`, `.pi`, `app`, `benches`, `ci`, `cmake`, `common`, `conversion`, `docs`, `examples`, `ggml`, `gguf-py` ... |
| [huggingface/candle](https://github.com/huggingface/candle) | `9c1fcffaf956fcfd629fdddc9fc25adefb95c052` | 2026-11-26 | Apache-2.0 | `.cargo`, `.github`, `candle-book`, `candle-core`, `candle-datasets`, `candle-examples`, `candle-flash-attn`, `candle-flash-attn-v3`, `candle-kernels`, `candle-metal-kernels`, `candle-nn`, `candle-onnx`, `candle-pyo3`, `candle-transformers` ... |
| [ml-explore/mlx](https://github.com/ml-explore/mlx) | `2e094ae588736f5fd73ea084b44b51a16a443526` | 2026-11-28 | MIT | `.github`, `benchmarks`, `cmake`, `docs`, `examples`, `mlx`, `python`, `tests` |
| [ml-explore/mlx-lm](https://github.com/ml-explore/mlx-lm) | `6fe0ce48c90cec11f2c305830ceba461051c33b2` | 2026-11-24 | MIT | `.github`, `benchmarks`, `examples`, `mlx_lm`, `tests` |
| [quickwit-oss/tantivy](https://github.com/quickwit-oss/tantivy) | `c005b68425675a1a022f5c28c872d932260e454b` | 2026-11-26 | MIT | `.claude`, `.github`, `benches`, `bitpacker`, `columnar`, `common`, `doc`, `examples`, `jitexpr`, `ownedbytes`, `query-grammar`, `src`, `sstable`, `stacker` ... |
| [scip-code/scip](https://github.com/scip-code/scip) | `8fe503757e133d98312b059b89e0ad5f72852d53` | 2026-11-21 | Apache-2.0 | `.github`, `bindings`, `cmd`, `docs`, `reprolang` |

### Unavailable

- `AmethystDev-Labs/QuickCLI`: not found (git and GitHub API, 2026-11-28).
- `microsoft/onnxruntime`: not found (git and GitHub API, 2026-11-28); use of its documentation is labelled "documentation only".

The commands that produced these pins are in [`evidence/ref-inventory-commands.md`](evidence/ref-inventory-commands.md) and the table with stars and archive status is [`evidence/ref-inventory.md`](evidence/ref-inventory.md).

## 3. Other sources

- Official documentation read only where a document says "documentation only"; URL and access date are given at the point of use.
- Prior research inside the OMP2 repository (for example `docs/research/protocols-schemas-serialization.md`, `docs/research/omp2-native-capabilities-and-composition/`) is treated as a claim to verify, never as evidence.

Back to the [index](README.md).
