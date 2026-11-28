# OMP2 local inference audit (area 10)

> Part of the [OMP2 gateway-capabilities research](../README.md). Evidence file for deliverable 10. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Status: first pass. The TSV twin is [`omp2-local-inference.tsv`](omp2-local-inference.tsv) (rows L01 to L16). Reference-repository facts are in [`ref-local-inference.md`](ref-local-inference.md).

## 0. Host facts (saved output)

Captured 2026-11-28 02:57 UTC on the research machine. Other work runs here, so the load average is high; every later measurement must record it again.

```
$ sysctl -n machdep.cpu.brand_string hw.memsize hw.ncpu
Apple M1 Pro
17179869184
10
$ uname -m
arm64
$ uptime
22:57  up 3 days, 11:21, 1 user, load averages: 24.15 21.84 18.42
$ sw_vers
ProductName:		macOS
ProductVersion:		27.2.1
BuildVersion:		26B131
$ sysctl hw.perflevel0.physicalcpu hw.perflevel1.physicalcpu
hw.perflevel0.physicalcpu: 8
hw.perflevel1.physicalcpu: 2
Sat Nov 28 02:57:48 UTC 2026
```

## 1. Scope read

Read: ADR 0023, 0042, 0057, 0060, 0062 (relevant parts only); `crates/ai/src/local/{mod,runtime,device,embedding,tiny_catalog,applefm}.rs` (outlines and key ranges); `crates/memory/src/embedding/*`; `crates/memory/src/{runtime,config,aux_lane}.rs` (relevant ranges); `crates/tools/src/memory.rs`; `crates/envd/src/memory.rs`; `crates/ai/src/discovery/{endpoints,probe}.rs`; `crates/roster-probe/src/lib.rs`. Not read: bodies of `parakeet.rs`, `stt.rs`, `tts.rs`, `artifact.rs`, `speech_catalog.rs`. No build, no test run, no model download.

## 2. Findings

1. **A lazy, generation-fenced embedding worker exists but nothing builds it.** `EmbeddingSupervisor` (`crates/memory/src/embedding/supervisor.rs:50`) holds one child behind a tokio mutex, spawns on first use and hard-reaps on timeout. A grep of `crates/` for `EmbeddingSupervisor::`, `SupervisorConfig` and `rebuild_local_embeddings` found definitions only. `rebuild_local_embeddings` (`crates/memory/src/runtime.rs:714`) has no caller. Recall passes `None` as the query vector in both tool paths (`crates/tools/src/memory.rs:261` `search`). Maturity: `Prototype`.
2. **The protocol is JSON lines.** Frames are `serde_json` with an 8 MiB cap, 256 texts per request and 4096 dimensions (`crates/memory/src/embedding/protocol.rs:12`, `:14`, `:18`). Vectors travel as JSON floats, so there is no binary tensor path.
3. **Two parallel fastembed integrations.** `omp-ai` has `EmbeddingAdapter` over `LocalRuntime` (`crates/ai/src/local/embedding.rs:195`) with no caller found. `omp-memory` has its own worker that loads `fastembed::TextEmbedding` directly (`crates/memory/src/embedding/worker.rs:8`). The adapter would load ONNX Runtime inside the OMP process; the worker confines it to a child process.
4. **Who calls the tiny model.** The catalog symbols are referenced only by the `omp tiny-models` installer (`crates/app/src/tiny_models_cmd.rs:40` `unique_models`). The only production caller of the `tiny` role is speech rewriting, which resolves `@tiny` to an online model (ADR 0023 status text, `docs/adr/0023-tiny-local-model.md:62`). The catalog lists 5 title and 5 memory GGUF `Q4_K_M` artifacts, from 229 MB (`lfm2-350m`) to 2.0 GB (`llama3.2:3b`) (`crates/ai/src/local/tiny_catalog.rs:101`, `:180`). No generator runs them.
5. **`TinyDevice` is dead.** It names `webgpu`, `dml`, `coreml`, `webnn` and maps to "llama.cpp's compiled layer-offload control" (`crates/ai/src/local/device.rs:83`), but a grep of `crates/` found no reference outside its file. Real device choice is hard-coded per engine (`crates/ai/src/local/stt.rs:433` `whisper_device`, `crates/ai/src/local/parakeet.rs:240`, `crates/ai/src/local/tts.rs:397`).
6. **Shared lifecycle exists; a shared budget does not.** `LocalRuntime` is `Arc`-cloned, lazy and serialized (concurrency must be 1), with idle unload (`crates/ai/src/local/runtime.rs:246`, `:258`, `:336`). `MemoryPool` is created by each consumer: 2 GiB in voice chat (`crates/app/src/chat_voice.rs:834`), `resident_bytes` in say (`crates/app/src/say_cmd.rs:60`) and in envd TTS (`crates/envd/src/media_tts.rs:482`). The pools cannot see each other. Apple FM has its own process-global admission of one (`crates/ai/src/local/applefm.rs:100` `APPLE_ADMISSION`).
7. **Subagents.** Subagents are child kernels in the same process (deliverable 2, section 8), and `MemoryRuntime` is one `Arc` per top-level session shared with them (`crates/envd/src/memory.rs:101` `session`). Today they share, not duplicate, whatever lives in `MemoryRuntime`. The embedding supervisor is caller-owned and not inside `MemoryRuntime`, so a future caller must choose where it lives. Two OMP processes (two terminals, headless runs) would each spawn a worker and each load the weights. Not measured.
8. **Apple FM is already a native on-device generator without a C binding.** The framework is loaded dynamically through a hand-written ABI seam (`crates/ai/src/local/applefm/aarch64.s`, `crates/ai/src/local/applefm.rs:3`). The OS chooses CPU, GPU or ANE placement; OMP cannot steer it. Context is 4096 tokens (`crates/ai/src/local/applefm.rs:99`). ADR 0042 (PCC) is remote inference reached through this boundary and is blocked (`docs/adr/0042-apple-pcc-integration.md:7`).
9. **A reranker prototype exists.** `rank` asks an Apple FM session for a 0 to 10 relevance score per candidate and falls back to input order (`crates/memory/src/aux_lane.rs:17`). It is gated by `apple-aux-memory` and only tests call it; `crates/memory/src/runtime.rs:439` names it as the future M4 step.
10. **llama.cpp, Ollama and LM Studio are already supported as local servers**, by loopback discovery on 8080, 11434 and 1234 (`crates/ai/src/discovery/endpoints.rs:113` `known_loopback_endpoints`) and `/props` context probing (`crates/ai/src/discovery/probe.rs:896` `apply_llama_props`). The catalog has `llama.cpp`, `lm-studio`, `ollama` and `vllm` provider entries (`crates/catalog/compat/providers/llama.cpp.kdl:3`). That is the local-server class and needs no new runtime. The typed client already has an `Embed` operation (`crates/ai/src/client.rs:70` `EmbedRequest`, `crates/ai/src/operation/embedding.rs:54` `EmbeddingService`); whether a local route serves it end to end was not verified.
11. **`crates/roster-probe` is not inference.** It registers a hidden test capability through the roster extension (`crates/roster-probe/src/lib.rs:63` `register_probe`). It holds nothing for this area.
12. **ADR 0057 `/local` means local files**, not local inference (`docs/adr/0057-session-tools.md:7`). ADR 0060 rejects an embedding index for semantic find (`docs/adr/0060-semantic-find.md:86`). ADR 0062 keeps fact extraction model-free and defers model lanes (`docs/adr/0062-mnemopi-fact-extraction.md:390`), which fits the deterministic-first rule.
13. **Semantic compression** exists as a standalone command that uses the configured session model (`crates/driver/src/compress/mod.rs:1`); no local-model path was read.
14. **Locked rule versus the graph.** `AGENTS.md:576` says local audio and ML use candle, never C or C++ binding graphs. `fastembed` pulls ONNX Runtime and downloads its binaries (`Cargo.toml:60`). It is opt-in and, in the memory worker, process-isolated, but it is not candle. Candle is enabled with the `metal` feature at workspace level (`Cargo.toml:42`).

## 3. Not verified

- Linux builds of the local features (no build run).
- Real memory and latency of any model (no measurement; plan in deliverable 10).
- Whether an `Embed` route to a local server works end to end.
- Callers in `extensions/` or Python preludes of any local ML symbol.

Back to the [index](../README.md).
