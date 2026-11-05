# Minimizer savings: what wiring `omp-minimizer` into bash output would save

Status: research; nothing here is approved or implemented
Date: 2026-11-05

Baseline: PR #515 head `df8b485bcf`, which is `omp2` at `69895da5a4` plus #515's 59 defs. `omp2` has
since merged more filters (#513), which the measured tree lacks; see
[What this study did not measure](#what-this-study-did-not-measure). The minimizer stays unwired: the
measurement calls `omp_minimizer::apply` from an example and models the output path around it.

The wiring slice is gated on before/after token evidence. This report supplies it for the case that
matters, output small enough to reach the model whole today. The per-capture tables are in
[`minimizer-savings/tables.md`](minimizer-savings/tables.md).

Evidence labels:

- **MEASURED**: computed in this session by the `savings` example on a BuildBuddy runner, or by the
  frame probe on the local Mac. `docs/research/minimizer-savings/measure.sh` rebuilds `tables.md`.
- **OBSERVED**: read in this repository at the baseline. The citation is `path:line`.
- **[INFERENCE]**: reasoned from measured or observed facts, not run.
- **NOT MEASURED**: listed in [What this study did not measure](#what-this-study-did-not-measure).

## Recommendation: wire it selectively, and only after four filter fixes

Do not turn the whole minimizer on. Wire the cargo filter for test runs and builds once the defects
below are fixed, and keep every other program off until it shows a saving that does not cost the
outcome.

1. **The headline saving is large but concentrated.** On the 66 captures that reach the model whole
   today (50 KiB or less), the minimizer cuts model-visible tokens from 157,868 to 69,021, a 56.3%
   saving. Five captures supply 67% of it. Four of those five hide what the command was run to show:
   two `just` runs lose their result, a `cat` of a source file becomes a list of `use` lines, and a
   failed build loses 5 of its 12 error locations. Without those rewrites the saving is about 26%.
   MEASURED (the five-capture split is arithmetic on the per-capture rows).
2. **Most captures do not change.** 25 of the 66 are under the engine's 1,000-character floor, and
   16 more pass through unchanged. The median capture of 16 to 50 KiB saves 0.0%; the median capture
   of 1,000 characters to 16 KiB saves 21.6%. MEASURED.
3. **Successful cargo test runs are the clean win.** A 47,841-byte `cargo nextest run -p` reaches the
   model as 13,959 tokens today and as 73 with the minimizer: the `Summary` line and the artifact
   reference. The five e2e runs of 50 to 64 KiB, one of them failing, drop from 46,573 tokens to 761.
   A passing run loses nothing the agent needs, because the bash tool's status line carries the exit
   code. MEASURED.
4. **Large failing builds gain what today hides.** A 1.4 MB failing `cargo clippy` shows none of its
   11 errors today, because they sit past envd's 64 KiB prefix. The minimizer shows all 11. MEASURED.
5. **The saving does not depend on the vocabulary.** The 50 KiB-or-less saving is 56.3% in o200k
   tokens, 56.4% with the legacy Claude vocabulary and 56.7% with the repository's own bytes/4
   estimate. MEASURED.

The four fixes, each found in this corpus:

- **nextest keeps only `FAIL` results** (`crates/minimizer/src/filters/cargo.rs:286-330`). All three
  failing workspace test runs failed by `TIMEOUT` or `SIGKILL`. The rewrite keeps the `Summary` count
  and drops the line that names the test, so 0 of 2 failure lines are visible. Today's prefix shows
  neither. MEASURED and OBSERVED.
- **The build head/tail cap drops errors that fit inline.** A 46,001-byte failed
  `cargo nextest run --no-run` reaches the model whole today, with all 12 error locations. The
  rewrite keeps 120 head and 60 tail lines (`condense_build` in `cargo.rs`) and shows 7. A cap must
  not remove errors from output that would have been shown in full. MEASURED.
- **The `just` def keeps the first 80 lines** (`crates/minimizer/src/defs/just.toml`,
  `primitives::max_lines` at `crates/minimizer/src/primitives.rs:360-375`). `AGENTS.md` makes `just`
  the way to run cargo here. `just test-pkg omp-minimizer` keeps compile warnings and the first `PASS`
  lines, then elides 467 lines, the `Summary` among them. `just remote check-pkg` keeps the
  BuildBuddy preamble and drops the whole cargo result. Route `just <recipe>` to the recipe's own
  filter, or keep `just` off. MEASURED.
- **Small rewrites cost more than they save.** The `gh` rewrites save -10.9% to -1.1%, because the
  artifact reference the caller must add outweighs what they remove. Across all 15
  `gh` captures the minimizer adds 0.2%. MEASURED.

What would change this recommendation is listed [at the end](#what-would-change-the-recommendation).

## What the model sees today

The brief put the inline threshold at 64 KiB. The production path has two thresholds, and the
smaller one is 50 KiB. OBSERVED:

- **envd keeps a prefix.** A bounded bash command projects its first 64 KiB or its first 64 read
  frames, whichever comes first, and spills the rest to an artifact
  (`crates/envd/src/exec.rs:89-90`, `exec.rs:3315-3334`, test
  `tiny_output_frames_are_count_bounded_and_spilled_whole`). The projection is head only: the end of a
  longer output, where test summaries and build errors sit, is not inline.
- **The bash tool adds a status line and a notice.** The model text is
  `[status=...; exit=...; ...ms]`, the transcript, and for spilled output
  `[full output: artifact://sha256/<hex>; <n> bytes]` (`crates/tools/src/shell.rs:998-1017`,
  `shell.rs:1248-1254`).
- **The dispatcher bounds the result.** The production kernel replaces
  `DispatchPolicy::DEFAULT_MAX_OUTPUT_BYTES` (64 KiB, `crates/agent/src/dispatch.rs:107`) with the
  settings (`crates/driver/src/headless/kernel.rs:2954-2975`): inline limit 50 KiB
  (`sv_tools_output_spill_bytes`, `crates/envd/src/tool_settings.rs:227-228`), lines cut at 768 bytes
  (`sv_tools_output_max_columns`, `crates/tools/src/settings.rs:220-221`), and above the limit a
  20 KiB head and a 20 KiB, 500-line tail (`settings.rs:157-158`, `187-188`, `243-244`;
  `artifact_projection`, `dispatch.rs:3547`).
- **The command runs without color.** The bash environment sets `NO_COLOR=1`, `TERM=dumb`, `CI=true`
  and pager overrides (`crates/envd/src/tool_shell.rs:230-275`). `bb remote` output still arrives
  colored, because the remote runner does not see that environment. MEASURED.

Two consequences showed up in the measurement. A 26,360-byte `gh api` response is one JSON line, so
the 768-byte line cap shows the model 242 tokens of it today. And the 64-frame cap binds before the
64 KiB cap when output arrives in many small writes: the local frame probe read
`uv run --with pytest pytest -v` as 64 frames for 2,831 bytes, one test line per frame. Of the other
eight probed commands, six (`git diff`, `gh pr diff`, `rg`, `cat`, `gh run view --log-failed` and
`just remote test-pkg`) hit the byte cap first and two stayed under both caps. MEASURED. A cold local `cargo build`
writes one `Compiling` line per crate and would be cut near its 64th line. [INFERENCE] The probe did
not run local cargo.

## Method

**Corpus.** 92 captures of real output from this repository's workflows, listed with command, exit
code and source in [`minimizer-savings/manifest.tsv`](minimizer-savings/manifest.tsv). The bytes are
in `minimizer-savings/captures.tar.gz` (4.6 MB compressed, about 30 MB of text). No capture is
hand-written.

- **57 local commands on the Mac:** `git`, `gh`, `ls`, `find`, `tree`, `cat`, `rg`, `grep`, `wc`,
  `du`, the two CI Python checks, `pytest` through `uv run`, and `just tools-test` (Bun). Each ran
  through `bash -c` in a non-PTY child with the bash tool's environment (`tool_shell.rs:230-275`),
  stdout and stderr merged, recording the command's own exit status. PR diffs use fixed commit IDs,
  so they reproduce exactly.
- **2 BuildBuddy runs and 5 slices of them:** `just remote check-pkg omp-minimizer` and
  `just remote test-pkg omp-minimizer` as an agent sees them, colors included (captured on the Mac,
  so the manifest labels them `local-macos`), plus the inner
  `just check-pkg`, `cargo check -p`, `just test-pkg`, `cargo nextest run -p` and `cargo test --doc -p`
  output cut from between the runner's `===` markers, with ANSI escapes removed.
- **28 CI step outputs** from 18 job logs of `ci.yml`, success and failure: workspace `nextest`,
  doctests, `--no-run` builds, `clippy` on both platforms, `cargo fmt --check`, the e2e proofs, lintx,
  the license check and `scripts/remote-verify.sh remote-heavy`. Each is the step's lines with the
  runner's timestamp prefix and the ANSI escapes removed (CI sets `CARGO_TERM_COLOR=always`; the bash
  tool sets `NO_COLOR`). The exit code is the runner's `Process completed with exit code` line, 0 if
  absent. One success per step command was kept, plus every failure.

**Minimizer.** `omp_minimizer::apply(command, capture, exit, &MinimizerConfig { enabled: true,
..Default::default() })`: the convar defaults with the master switch on.

**What the model sees.** `crates/minimizer/examples/savings.rs` models both paths from the code
cited above. Today: envd's 64 KiB prefix and spill notice, then the 768-byte line cut and the 50 KiB
head/tail projection. With the minimizer: the rewrite, the same notice pointing at the original (the
minimizer contract asks the caller to persist it and splice a reference), then the same dispatcher
bound. Unchanged captures cost the same on both paths. The status line is identical on both and is
not counted. The model ignores the 64-frame cap and event backpressure; both can only shorten
today's view.

**Tokens.** The `tokenizers` crate at the workspace version and features (0.22.2, `unstable_wasm`).
The repository ships no vocabulary, and Anthropic publishes none for current Claude models. The
primary vocabulary is `o200k_base`, a current production BPE, from `Xenova/gpt-4o` at revision
`7956d98f2a83` (SHA-256 `43a3ad46...`). The cross-checks are the legacy Claude vocabulary from
`Xenova/claude-tokenizer` at `cae688821ea0` (SHA-256 `c241737d...`) and the repository's own
`bytes/4` estimate (`crates/agent/src/context.rs:24`). `measure.sh` pins both downloads by revision
and checks the full hashes.

**Failures.** For each failing capture (exit code not 0) the example counts the distinct failures:
`error...:` lines at the line start keyed by the `-->` location under them, nextest results of every
failing kind, libtest `FAILED` lines, panics and `Diff in` lines. A failure is visible when its key
appears in the model's text.

## Results

MEASURED, o200k tokens. Saved is the share of today's model-visible tokens.

| Size of the capture | Captures | Rewritten | Today | Minimized | Saved | Per capture: min / median / max |
|---|--:|--:|--:|--:|--:|---|
| under 1,000 chars | 25 | 0 | 2,300 | 2,300 | 0.0% | 0.0% / 0.0% / 0.0% |
| 1,000 chars to 16 KiB | 29 | 19 | 40,519 | 24,391 | 39.8% | -10.9% / 21.6% / 87.8% |
| 16 KiB to 50 KiB | 12 | 6 | 115,049 | 42,330 | 63.2% | 0.0% / 0.0% / 99.5% |
| **50 KiB or less, whole today** | **66** | **25** | **157,868** | **69,021** | **56.3%** | |
| 50 KiB to 64 KiB | 5 | 5 | 46,573 | 761 | 98.4% | 95.2% / 99.2% / 99.2% |
| **64 KiB or less** | **71** | **30** | **204,441** | **69,782** | **65.9%** | |
| over 64 KiB | 21 | 13 | 239,544 | 133,709 | 44.2% | -9.3% / 78.1% / 99.3% |
| **all** | **92** | **43** | **443,985** | **203,491** | **54.2%** | |

The 21 captures over 64 KiB hold 9,509,998 tokens of raw output. Today the model sees 239,544 of
them.

| Program | Captures | Rewritten | Today | Minimized | Saved |
|---|--:|--:|--:|--:|--:|
| `cargo` | 24 | 20 | 186,762 | 30,123 | 83.9% |
| `just` | 6 | 5 | 43,815 | 6,239 | 85.8% |
| `cat` | 5 | 2 | 39,434 | 18,619 | 52.8% |
| `git` | 19 | 6 | 28,484 | 12,224 | 57.1% |
| `rg` and `grep` | 5 | 3 | 10,237 | 2,439 | 76.2% |
| `ls`, `find`, `tree`, `du`, `wc` | 8 | 4 | 4,340 | 2,781 | 35.9% |
| `gh` | 15 | 3 | 84,023 | 84,176 | -0.2% |
| `scripts/remote-verify.sh` and other scripts | 5 | 0 | 45,986 | 45,986 | 0.0% |
| `python3` and `uv` | 5 | 0 | 904 | 904 | 0.0% |

Failures in the failing captures, visible today and with the minimizer:

| Capture | Filter | Failures | Today | Minimized |
|---|---|--:|--:|--:|
| failed `cargo nextest run --no-run` (compile errors, 46,001 bytes) | `cargo` | 12 | 12 | 7 |
| failed e2e P7 run (55,656 bytes) | `cargo` | 4 | 4 | 3 |
| failed workspace `nextest` runs, 3 of them (`TIMEOUT`, `SIGKILL`) | `cargo` | 2 each | 0 | 0 |
| failed `cargo clippy` (1,445,872 bytes) | `cargo` | 11 | 0 | 11 |
| failed `remote-heavy`, 2 of them | unsupported | 4 and 8 | 0 | 0 |
| failed `cargo fmt --check` (463 bytes) | under the floor | 1 | 1 | 1 |

The three vocabularies agree:

| Counter | Saved, all | Saved, 50 KiB or less |
|---|--:|--:|
| o200k | 54.2% | 56.3% |
| legacy Claude | 54.4% | 56.4% |
| bytes/4 | 55.6% | 56.7% |

### Where the saving comes from

The 88,847 tokens saved at 50 KiB or less come mostly from five captures. MEASURED:

| Capture | Today | Minimized | What the model would see |
|---|--:|--:|---|
| `cargo nextest run -p omp-minimizer` (pass) | 13,959 | 73 | the `Summary` line |
| `cat crates/minimizer/src/engine.rs` | 13,753 | 566 | the file's `use` and `mod` lines |
| `just test-pkg omp-minimizer` (pass) | 14,550 | 1,686 | warnings and 60 `PASS` lines; no `Summary` |
| failed `cargo nextest run --no-run` | 12,079 | 1,588 | 7 of the 12 error locations |
| `just remote check-pkg omp-minimizer` | 10,204 | 1,433 | the BuildBuddy preamble; no cargo result |

Only the first is a saving without a cost. Excluding the `just` def rewrites, the `cat` outline and
the failed build leaves 41,763 tokens saved, 26.5% of today's 157,868.

## What this study did not measure

- **The field command mix.** The aggregate saving depends on how often agents run each command and
  at what size. This corpus is a spread of this repository's workflows, not a sample of sessions.
  NOT MEASURED.
- **Re-reads.** A rewrite that sends the agent to the artifact costs a second tool call and its
  tokens. `primitives.rs:39-44` records an upstream observation that nearly half of summarized `jq`
  results and patches were followed by a raw read; this study did not verify it. NOT MEASURED.
- **Interactive sessions and model quality.** No session ran with the minimizer, and no answer was
  graded. NOT MEASURED.
- **Chains and pipes.** Every capture is one command. The engine passes `cd x && ...` and `... | tail`
  through as compound or piped (`crates/minimizer/src/engine.rs`), so agent habits that add them
  would lower the saving. [INFERENCE]
- **Local cargo.** The self-hosted runner was busy for the whole session, so cargo ran only on
  BuildBuddy and in CI. Local line-streamed cargo output and the 64-frame cap were not probed.
  NOT MEASURED.
- **`docker build` and npm.** The Docker daemon was not running, and the repository has no npm
  project; Bun's `tools-test` stands in. NOT MEASURED.
- **Exact Claude counts.** The current Claude tokenizer is not published, and no API key was
  available for `count_tokens`. The three counters above bound the vocabulary effect. NOT MEASURED.
- **Filters merged after the baseline.** `omp2` merged more filters in #513 (`0f98b515f2`). Its engine
  labels add `pytest`, `ruff`, `mypy`, `bun`, `cmake` and `ctest`, among others. #513 conflicts with
  #515 in `engine.rs` and `filters/mod.rs`, so the measured tree lacks them. In this corpus only
  `uv run --with pytest pytest -v` (764 tokens today) could reach one of them. The other Python and Bun
  captures are under the 1,000-character floor. Even a rewrite to nothing would change the saving on
  output of 50 KiB or less by under 0.5 points (764 of 157,868 tokens). [INFERENCE]

## What would change the recommendation

- **Toward wiring more:** a session sample in which cargo and `just` runs dominate the command mix,
  and the four fixes in place. The successful-run and large-failure wins then carry the aggregate.
- **Toward wiring less:** a re-read rate near the upstream figure for the rewritten programs, or any
  failing run whose failure the fixed filters still hide.
- **Toward not wiring:** if the 64-frame cap and the head-only envd prefix are fixed first, today's
  view of long test runs gains its tail, and the remaining saving is the success summary alone.
  [INFERENCE]

## Reproduce

`docs/research/minimizer-savings/measure.sh > docs/research/minimizer-savings/tables.md` downloads
and checks the two vocabularies, unpacks the captures and runs
`cargo run --profile release-dev -p omp-minimizer --example savings`. Extra arguments reach the
example: `--show=<capture id>` appends the text the model would see for that capture. This study
ran it on BuildBuddy with
`scripts/remote-verify.sh --command bash docs/research/minimizer-savings/measure.sh`.