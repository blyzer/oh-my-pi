# 0080. Local inference placement and shared residency: out-of-process C and C++ runtimes, one memory budget per process

Status: proposed (draft written by the gateway-capabilities research; not owner-reviewed, not approved)
Date: 2026-11-28
Area: inference

Read against `omp2` at `85d219ae7f`. Number 0080 is provisional: it is the next free number on that commit and may be taken by another record; the owner may renumber.

**Review status.** Draft. One agent read the code and wrote this from the capability cards named below in [`docs/research/gateway-capabilities/`](../research/gateway-capabilities/README.md). No machine review, no owner review, nothing measured. It changes no existing ADR; where it would amend one, that is a proposal for the owner to apply.

## Context

The locked rule says local ML goes through candle and never through C or C++ binding graphs (`AGENTS.md`, "Locked Deviations from pi"). The local-inference audit ([`evidence/omp2-local-inference.md`](../research/gateway-capabilities/evidence/omp2-local-inference.md)) found: `LocalRuntime` shares one lazily loaded engine across clones and unloads on idle (`crates/ai/src/local/runtime.rs:246`, `:336`); memory pools are made per consumer (`crates/app/src/chat_voice.rs:834`, `crates/app/src/say_cmd.rs:60`, `crates/envd/src/media_tts.rs:482`) and Apple FM has its own admission (`crates/ai/src/local/applefm.rs:100`), so nothing bounds the sum on a 16 GB machine; the Mnemopi embedding worker exists (`crates/memory/src/embedding/supervisor.rs:50`) but nothing builds it in production; `TinyDevice` has no references outside its own file; the opt-in `local-embedding` feature already pulls `fastembed`, which downloads ONNX Runtime (`Cargo.toml:60`). llama.cpp as a local server already works through loopback discovery without a new binding graph.

## Decision

1. **The process boundary is the dividing line**: C and C++ runtimes are allowed only out of process (a local server or a managed subprocess, classes `Suitable as an external integration` or `Suitable as an optional extension`: INF-03, INF-05, INF-07) and in process only through a native OS framework seam. In-process llama.cpp stays rejected unless the owner reopens the locked rule (INF-04).
2. **One `MemoryPool` per process** and a per-machine lock or socket for the embedding worker, reported through existing status surfaces. Weights are shared; KV and generation state stay per request or per slot (INF-02).
3. **A typed hardware placement policy** (one enum, one resolver) replaces the dead `TinyDevice` (INF-10).

## Consequences

Two OMP processes load one embedding model between them; subagents in one process add no resident copy; a failed reservation returns `LocalErrorKind::Overloaded`. The `fastembed` ONNX path is either confined to the out-of-process worker or removed (INF-07).

## Alternatives considered

Per-consumer pools; a separate inference daemon with its own scheduler (redundant, owner rule 8); relying on OS paging; in-process llama.cpp (contradicts the locked rule).

## Acceptance (from the cards)

Two concurrent OMP processes with the feature on load one embedding model; a failed reservation returns `LocalErrorKind::Overloaded`; subagents in one process add no extra resident copy; tests cover a crash of the owner. Benchmarks B1 and B2 in [deliverable 10](../research/gateway-capabilities/10-local-inference-assessment.md) come first.

## Owner questions

O1. Is semantic recall over local embeddings a product intent (OQ-08)? O2. Accept the process boundary rule and decide on `fastembed` (OQ-07). O3. Approve one memory budget (OQ-09).

## References

Cards INF-01, INF-02, INF-03, INF-04, INF-05, INF-07, INF-10 in [deliverable 10](../research/gateway-capabilities/10-local-inference-assessment.md); ADR 0023; ADR 0042; ADR 0049.
