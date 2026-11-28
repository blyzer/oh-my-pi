# Reference repositories: local inference

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for deliverable 10. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Reference clones are untrusted input and were read, never built or run. TSV twins: `matrix-ggml-org_llama.cpp.tsv`, `matrix-ml-explore_mlx.tsv`, `matrix-ml-explore_mlx-lm.tsv`, `matrix-huggingface_candle.tsv`, `matrix-apple_coremltools.tsv`. Licences are in [`ref-inventory.md`](ref-inventory.md). No code was copied.

## How deeply each was read

| Repository | Depth | Consequence |
|---|---|---|
| `ggml-org/llama.cpp@8e23278` | Server README headings and option tables; `tools/server/` file list; a few symbols in `server-context.cpp` | Firm where a code line is cited, documentation-level otherwise |
| `ml-explore/mlx@2e094ae` | Top-level and `mlx/backend` listing; `unified_memory.rst`; README grep | Architecture only |
| `ml-explore/mlx-lm@6fe0ce4` | `mlx_lm` listing; symbol grep in `server.py` and `models/cache.py` | Class names only; no body read |
| `huggingface/candle@9c1fcff` | Crate and model file listing; `new_metal` symbol | Architecture only; model quality and speed unknown |
| `apple/coremltools@a207b1a` | Package listing; `ComputeUnit` docs in `models/model.py` | Conversion tooling facts only |
| ONNX Runtime | Official docs pages only (CoreML execution provider, threading). **Documentation only, code not read.** The `microsoft/onnxruntime` clone was unavailable (see `ref-inventory.md`) | Claims are documentation claims |

## llama.cpp

- **Server shape.** `llama-server` exposes native routes (`/completion`, `/embedding`, `/reranking`, `/slots`, `/props`, `/metrics`), OpenAI-style routes (`/v1/chat/completions`, `/v1/embeddings`, `/v1/responses`) and an Anthropic-style route (`/v1/messages`) (`ggml-org/llama.cpp@8e23278:tools/server/README.md`). Used by OMP2 it is a local server, not an in-process library.
- **Weights versus KV state.** One loaded model serves `--parallel` slots. Each slot is a `server_slot` with its own context and KV (`ggml-org/llama.cpp@8e23278:tools/server/server-context.cpp`, struct at line 267). `--kv-unified-per-slot` sizes one shared KV pool (line 1473). Slot prompt caches can be saved, restored and erased through `/slots/{id}?action=` (README). Weights are shared across requests; generation state is per slot and must not be treated as shared between agents.
- **Embeddings and rerank.** `/v1/embeddings` and `/embedding` need an embedding model. `/reranking` needs a rerank model with pooling rank, and the server rejects a causal-attention model for rank pooling (`ggml-org/llama.cpp@8e23278:tools/server/server-context.cpp:534`).
- **Residency control.** Router mode loads several models, evicts least recently used, and with `--models-memory-margin` unloads idle models until a new one fits (`ggml-org/llama.cpp@8e23278:tools/server/server-models-sched.cpp`; README "Memory-based unloading"). A "Sleeping on Idle" section exists (README only).
- **Platform.** Builds and runs on Unix without Docker (README quick start). Metal and CUDA backends exist (README options `-ngl`, `-dev`).

## MLX and mlx-lm

- MLX is an array framework with CPU, Metal and CUDA backends and an explicit unified-memory model (`ml-explore/mlx@2e094ae:docs/src/usage/unified_memory.rst`). The core is C++ and the usual entry is Python. A grep found no Neural Engine path.
- mlx-lm's server keeps one model and an `LRUPromptCache` of per-sequence KV caches (`ml-explore/mlx-lm@6fe0ce4:mlx_lm/server.py`, lines 47 and 543). Cache classes `KVCache`, `RotatingKVCache`, `QuantizedKVCache` and `BatchKVCache` live in `mlx_lm/models/cache.py`. Same split: shared weights, per-sequence KV.
- A Python plus C++ runtime can only enter OMP2 out of process.

## Candle

- Rust tensor library with CPU, CUDA and Metal devices (`huggingface/candle@9c1fcff:candle-core/src/device.rs`, `new_metal` at line 244, plus `candle-metal-kernels`). `candle-transformers/src/models/` holds `bert.rs`, `quantized_llama.rs` and `quantized_lfm2.rs`. LFM2 is the family OMP2's tiny catalog prefers (`crates/ai/src/local/tiny_catalog.rs:101`), so a candle path for the smallest catalog model is plausible. Speed and quality against llama.cpp: not measured.

## coremltools

- Python tooling that converts models to Core ML and wraps prediction. `ComputeUnit.CPU_AND_NE` is the documented way to allow the Neural Engine, gated to macOS 13 or later (`apple/coremltools@a207b1a:coremltools/models/model.py`, lines 425 to 428 and 505). OMP2 cannot link it; the runtime is the OS Core ML framework.

## ONNX Runtime (documentation only, code not read)

- The CoreML execution provider takes `ModelFormat` (`MLProgram` or `NeuralNetwork`), `MLComputeUnits` (including `ALL`), `RequireStaticInputShapes` and `EnableOnSubgraphs`, plus a deprecated flag set that includes an ANE-only-devices flag (onnxruntime.ai, CoreML Execution Provider page, fetched 2026-11-28). Unsupported operators fall back to the CPU provider, so partial ANE use should be expected (documentation claim).
- `intra_op_num_threads` defaults to 0 (runtime chooses) and `allow_spinning` is a session config entry (onnxruntime.ai, threading page). Spinning threads matter on a shared 10-core, 16 GB machine; not measured.

Back to the [index](../README.md).
