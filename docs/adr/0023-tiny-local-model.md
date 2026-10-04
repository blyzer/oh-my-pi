# 0023. An embedded tiny model handles harness chores

Status: accepted
Date: 2026-09-02
Area: inference

## Context

A harness generates a steady stream of small language tasks that have nothing to do with the user's
problem: classify this prompt's difficulty, title this session, translate this notice, judge whether
the user is getting frustrated, transcribe this microphone buffer, speak this reply. Sending each of
these to the frontier model pays frontier latency and frontier cost for a 300-token job, and does so
on every turn.

Sub-billion-parameter models (the playbook names LiquidAI's LFM2 line) answer these tasks well
enough when their output is constrained to a small ladder, and local speech models already reach
state-of-the-art quality for TTS and STT. Even a harness that only ever talks to frontier models
benefits from carrying one.

## Decision

The harness MUST route small internal chores (classification, title generation, translation,
sentiment, memory extraction, speech rewriting, and speech in and out) to a dedicated `tiny` model
role, never to the user's frontier model by default.

1. The `tiny` role resolves to a configured model. An explicit role assignment wins; with none, it
   resolves to the `commit` role and then to `smol` (`crates/catalog/src/selection.rs`). The
   default is therefore an online model chosen by routing. An in-process tiny text generator is a
   future option, not current behavior; no embedded text model ships or runs today.
2. The tiny role is NEVER a second agent. It has no tools, no session, and no place in the
   transcript. It is a bounded internal operation with a fixed output ladder and earliest-match
   parsing, never free prose that a downstream parser has to trust.
3. Local inference that does exist (speech, the opt-in Apple Foundation Models route, and any future
   embedded generator) runs in-process under the same admission, memory, cancellation, and
   idle-unload lifecycle, with verified, root-confined model artifacts.
4. A pinned local tiny model is NEVER silently promoted to a hosted one (cost leak, privacy
   incident); fallback is an explicit caller policy. The default online resolution in rule 1 is a
   configured route, not a promotion.
5. Local ML runs on Rust-native runtimes (candle); C/C++ binding graphs (whisper-rs, llama-cpp) are
   prohibited (`AGENTS.md`, Runtime).

## Consequences

- Chores routed to the `tiny` role cost no frontier tokens. With the default online resolution
  they still add one small hosted round trip each; they do not add one to the frontier model.
- Local speech works offline. Text chores work offline only with a local backend, and no
  in-process text generator exists yet.
- Cost accepted: model artifacts for local speech are downloaded and verified once; the harness
  carries a local inference runtime and its memory reservation.

## Amendment (2026-10-04)

The owner decided to bring this record in line with the code. The decision originally required an
embedded tiny model, run in-process, to handle chores by default. No in-process tiny text
generator exists: the `tiny` role resolves through normal routing to a configured model, which is
an online model by default, and the local pieces that do exist are the artifact store, the
lifecycle runtime, local speech, and an opt-in Apple Foundation Models route. The decision now
states that. The embedded generator, running a curated GGUF-class model on a Rust-native runtime,
is kept as a future option and is not a current requirement. The cost and latency motive for a
separate chore role is unchanged and is met by the role itself.

## Status in omp

**Status: Partially implemented.** The `tiny` role, verified local artifacts, the lifecycle runtime and local speech exist; the role resolves to an online model by default; no in-process tiny text generator exists, and most named chores have no caller. (Verified 2026-10-04 against `omp2` at `9b2d91fe9d`.)

- Role: `AI_TINY_SELECTOR` (default `@tiny`) in `crates/catalog/src/settings.rs`; the `tiny` role falls back to `commit`, then `smol` when unassigned (`crates/catalog/src/selection.rs`, around lines 448 and 480). Speech rewriting resolves `@tiny` through `resolve_role_selector` and calls it (`speech_rewriter` and `SpeechRewriteClient::rewrite` in `crates/driver/src/headless/kernel.rs`; `crates/ai/src/realtime/rewrite.rs`).
- Local artifacts and lifecycle: curated GGUF artifact catalog and the `ONLINE_TINY_MODEL` sentinel in `crates/ai/src/local/tiny_catalog.rs`; verified artifact store and admission/idle-unload runtime in `crates/ai/src/local/{artifact,runtime}.rs`; `omp tiny-models` installer in `crates/app/src/tiny_models_cmd.rs`.
- Local speech: candle Whisper and Parakeet STT and Kokoro TTS in `crates/ai/src/local/{stt,parakeet,tts}.rs` behind `local-stt`/`local-tts`.
- Opt-in on-device text: an Apple Foundation Models route behind `local-applefm` (`crates/ai/src/local/applefm.rs`, bound as the `local` provider's `CodecProfile::AppleFm` in `crates/ai/src/provider/builtin.rs`).
- Not present: a candle or other in-process generator for the GGUF title, memory and classifier models in `tiny_catalog.rs`.
- Chores without a caller (checked by grep, 2026-10-04): the auto-thinking `DifficultyClassifier` in `crates/ai/src/difficulty.rs` builds an `@tiny` request, but nothing outside that module calls `classify_online`; the title helpers `is_low_signal_title_input` and `normalize_generated_title` in `crates/ai/src/local/title.rs` have no caller anywhere; the `memory_selector`, `auto_thinking_selector`, `unexpected_stop_selector` and `tiny_selector` fields of `ModelSettings` (`crates/catalog/src/settings.rs`) are filled from convars (`ai_memory_selector`, `ai_auto_thinking_selector`, `ai_unexpected_stop_selector`, `ai_tiny_selector`) and read nowhere else, so those convars, whose suggestions list local models, change nothing today.
- Stale code comments and dead code, for a follow-up cleanup (no `.rs` file is changed by this amendment): `crates/ai/src/local/mod.rs` line 13 carries `/// llama.cpp GGUF text generation.` above `pub mod message_preproc` with no module behind it, which `AGENTS.md` prohibits as a direction; line 8, `/// FastEmbed local embeddings.`, merges into the doc of `pub mod device`; `crates/ai/src/local/title.rs` lines 22 and 40 are the caller-less validators.

## References

- The Harness Playbook, "The inference" — "Use small local models for harness work"
- LiquidAI LFM2 (`huggingface.co/LiquidAI`)
- 0001, 0018
- `crates/ai/src/local/`, `crates/ai/Cargo.toml`, `AGENTS.md` (Runtime),
  `docs/py/13-inference.md`
