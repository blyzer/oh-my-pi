# Grammar bundling: keep the tree-sitter grammars compiled in

Status: research; nothing here is approved or implemented
Date: 2026-11-05

Baseline: `origin/omp2` at `f698bf6292`. Upstream comparison: `can1357/oh-my-pi` `main` at
`88a69582c7`.

This report decides, from measurements, whether `omp2` should keep its 56 compiled-in tree-sitter
grammar crates or move to a transportable model such as upstream's on-demand WebAssembly grammars.
It also weighs models that neither repository uses. The per-language tables are in
[`grammar-bundling/tables.md`](grammar-bundling/tables.md).

Evidence labels:

- **MEASURED**: timed or counted in this session on a BuildBuddy runner. The raw result lines are in
  `grammar-bundling/raw/`, `grammar-bundling/runs.tsv` names the run behind each file, and
  `python3 docs/research/grammar-bundling/analyze.py` rebuilds every table from them.
- **OBSERVED**: read in this repository, in upstream at the commit above, or in the registry source
  of a pinned crate. The citation is `path:line`.
- **[INFERENCE]**: reasoned from measured or observed facts, not run.
- **NOT MEASURED**: listed in [What this study did not measure](#what-this-study-did-not-measure).

## Recommendation: keep the grammars compiled in and add no WebAssembly runtime

Keep the 56 grammar crates compiled into `omp-ast`. Do not adopt upstream's WebAssembly model, and do
not add a WebAssembly runtime to `omp2` to load grammars.

1. **The WebAssembly model does not make `omp2` build faster.** In a cold `cargo build -p omp-ast`
   (dev profile, `-j 4`), the 39 long-tail grammars cost 18.8 s of wall time: 43.1 s with all 56
   grammars, 24.3 s with upstream's 17 linked ones. Adding the `tree-sitter/wasm` runtime to those 17
   costs 17.5 s (41.8 s). At `-j 11` the runtime costs 14.1 s against 14.3 s for the long tail. In
   the release profile the WebAssembly variant is slower than all-native, 69.9 s against 63.2 s.
   MEASURED.
2. **Grammars cost almost nothing on an incremental build.** A no-op build takes 0.36 s with all
   grammars and 0.35 s with none. Rebuilding `omp-ast` takes 1.97 s and 1.94 s. Relinking the dev
   `omp` binary takes about 0.1 s longer of 4.2 to 5.2 s. MEASURED.
3. **What the grammars cost is bytes.** They add 96.1 MB to every release binary that links
   `omp-ast`: `omp-kb` is 97.3 MB with all grammars and 1.2 MB with none. Compressed, the same
   grammars are 7.4 MB with `gzip -9` and 5.0 MB with `zstd -19`. MEASURED. That is about 30 % of
   the 319.6 MB macOS `omp` release binary and about 7 % of its 103.6 MB tarball. [INFERENCE: Linux
   bytes set against a macOS binary.]
4. **The WebAssembly model moves the cost to the user at run time.** The first use of a WebAssembly
   grammar in a process takes 22 to 675 ms (median 66 ms), mostly wasmtime compiling the module. The
   first download adds a median 246 ms from a datacenter. Parsing is 1.10 to 2.45 times slower
   (median 1.74). A WebAssembly store grows by up to 59 bytes for every byte it parses (median 14.5),
   so it has to be dropped and rebuilt on a budget. A native parser does not grow. MEASURED.

The trade is about 80 MB raw (about 4.3 MB gzip) per binary, paid for with slower and less
predictable parsing, a download path that fails offline, and a new runtime. It saves no build time.

### What would change this recommendation

- **A size budget that 96 MB raw or 7.4 MB compressed breaks.** Then move the long tail out of the
  binary. Prefer a smaller default set with build-time opt-in over a run-time loader, unless the
  next point also holds.
- **A need to load grammars that `omp2` does not ship,** such as user-supplied grammars. Then a
  sandboxed WebAssembly runtime is a feature, and its costs above buy something.
- **Field data that the 39 long-tail languages are rarely parsed.** Then the smaller default set is
  cheaper than any loader. `omp2` records no such data today.
- **A tree-sitter or wasmtime release that stops the per-call store growth and caches compiled
  modules on disk.** That removes the two worst run-time costs measured here.
- **Cold builds that happen far more often than this study assumes,** without `sccache`. Then
  build only the 17-grammar set in development and test builds. It cut the cold `omp-ast` build by
  18.8 s (44 %) at `-j 4`, and all 56 grammars cost the cold `omp` build a median 25.6 s.

### The cost of being wrong

- **If `omp2` keeps them and should have moved,** every cold build keeps paying the grammar compile:
  21.4 s of 43.1 s for `omp-ast`, and a median 25.6 s (6 %) of a cold `omp` dev build, both at
  `-j 4`. Every binary that links `omp-ast` keeps 96 MB of parse tables, and a full test build can
  hold up to about 10.7 GB of them in `target/` [INFERENCE: 111 targets times the measured
  per-binary bytes]. None of this compounds. Moving later costs what moving now costs, and
  upstream's loader stays available as a reference.
- **If `omp2` moves and should not have,** it owns a loader, a store budget, a download, verify and
  cache path, an offline path, and either a grammar build pipeline or a dependency on a third-party
  release (`stencil-hq/wasm-grammars`). Users get slower parsing for 39 languages, first-use stalls
  of up to 0.7 s per language per process, the memory growth that upstream hit as issue #16067, and
  missing results when offline. The build gets no faster. Reverting means re-adding the crates,
  which is cheap, after users depend on the download behaviour, which is not.

## Inventory: 56 crates compiled and referenced, 5 sub-grammars compiled for nothing

`omp2` declares 56 grammar crates (`Cargo.toml:255-310`) and `omp-ast` depends on all of them
(`crates/ast/Cargo.toml:24-79`). `crates/kb` depends on none directly. It parses through
`omp-ast`'s `SupportLang` (`crates/kb/src/graph/syntax.rs:177-193`), and it runs SCIP producers for
Rust, TypeScript, TSX, JavaScript and Python only (`crates/kb/src/producer.rs:58-74`). OBSERVED.

| Question | Answer | Evidence |
|---|---|---|
| Grammar crates compiled | 56 crates, 590.8 MiB of `parser.c` | OBSERVED, `Cargo.lock` and registry sources |
| Parser functions referenced | 57 (`tree-sitter-typescript` provides TypeScript and TSX) | OBSERVED, `crates/ast/src/language/parsers.rs` |
| Crates compiled but never referenced | none | every function above backs a `SupportLang` variant, `crates/ast/src/language/mod.rs:127-189` and `:224` |
| Sub-grammars compiled but never referenced | 5 parsers in 4 crates, 70.0 MiB of C (12 %) | see the next table |
| Grammar entry points linked into a binary | 57 with all grammars, 18 with the 17-crate set, 0 with none | MEASURED, `nm` on `omp-kb` and `omp`; 57 also in the local macOS dev `omp` of 2026-10-30 |
| Unreferenced sub-grammars linked | 0 in every binary checked | MEASURED, same `nm` runs |
| Crates whose binaries link `omp-ast` | 11: `omp-app`, `omp-ast`, `omp-chat`, `omp-driver`, `omp-e2e`, `omp-edit`, `omp-envd`, `omp-exec-local`, `omp-exec-ssh`, `omp-kb`, `omp-tools` | OBSERVED, `cargo metadata` |
| Targets in those crates that produce a binary | 111: 11 library test binaries, 6 bins, 94 integration tests | OBSERVED, `cargo metadata` |
| Languages parsed in the field | unknown | NOT MEASURED: `omp2` records no per-language usage |

Four crates build grammars that `omp-ast` never asks for. Each build script compiles every parser
in its crate, so the cost is compile time only, and the linker drops the objects.

| Crate | Compiled | Referenced by `omp-ast` | Unreferenced `parser.c` | Evidence |
|---|---|---|---|---|
| `tree-sitter-ocaml` 0.24.2 | ocaml, interface, type | `LANGUAGE_OCAML` | 60.8 MiB | `bindings/rust/build.rs:5-26`, `lib.rs:44-54` |
| `tree-sitter-php` 0.24.2 | php, php_only | `LANGUAGE_PHP_ONLY` | 6.9 MiB | `bindings/rust/build.rs:4-24`, `lib.rs:36-41` |
| `tree-sitter-md` 0.5.3 | block, inline | `LANGUAGE` (block) | 2.2 MiB | `bindings/rust/build.rs:2-39`, `lib.rs:25` |
| `tree-sitter-xml` 0.7.0 | xml, dtd | `LANGUAGE_XML` | 0.2 MiB | `bindings/rust/build.rs:3-29`, `lib.rs:40-45` |

This is the cheapest saving on paper, but it does not make the question smaller. The OCaml build
script is the longest grammar unit: 11.9 s at `-j 4`, and 15.5 s in the first `-j 11` repetition,
where it ends at 25.9 s and `omp-ast` starts right after it. The next grammar, Julia, ends at
24.1 s. Removing the two unused OCaml parsers would save about 2 s of wall time at either job count,
and it needs a vendored or patched crate. [INFERENCE from the MEASURED unit timings.]

## How the measurements were taken

- **Where.** Every timed build ran on a BuildBuddy remote runner through `scripts/remote-verify.sh`
  (`just remote grammar-study ...`): Linux x86_64, 11 CPUs by `nproc`, 16 GB, gcc 13.3,
  `rustc 1.99.0-nightly (1a98b1e13 2026-08-07)`. Each `just remote` run is one VM resumed from the
  `omp2` snapshot. No build, test or benchmark ran on the Mac.
- **No CI job competed.** The Mac is the runner that `vars.MACOS_RUNNER` selects
  (`["self-hosted","macOS","ARM64"]`, `gh variable list`). I checked it with
  `gh api repos/blyzer/oh-my-pi/actions/runners` and `gh run list --status in_progress`. At
  08:53 UTC it was idle with no run in progress, but other agents held its load average at 48. From
  09:22 UTC it ran "Rust workspace and acceptance proofs" for run 42001679729
  (`feat/cleanse-junit-parser`), and it was still busy at 10:09 UTC. For those reasons no number in
  this report comes from the Mac. A BuildBuddy VM runs one invocation, so no CI job shares it. Each
  cold build logged the VM's `/proc/loadavg` before it started.
- **Target directories.** Each cold build used a fresh
  `CARGO_TARGET_DIR=$HOME/.cache/grammar-study/<phase>-<variant>` on the runner, removed before and
  after the build. The runner's own `$HOME/.cache/omp-target` and the repository's `target/` were
  never used. The registry was warm, and `cargo fetch --locked` ran untimed before the first build.
- **Variants.** The harness branch `research/grammar-bundling-harness` (never merged) puts every
  grammar behind a Cargo feature: **all** (56 crates), **hot** (upstream's 17 linked crates,
  `crates/pi-ast/Cargo.toml` at `88a69582c7`), **none**, and **hot-wasm** (the 17 plus
  `tree-sitter/wasm`, which upstream enables by default). `omp-ast` and `omp-kb` builds pass
  `--config resolver.feature-unification="selected"`. The `omp` builds keep the workspace's own
  setting.
- **Repetition and order.** Variants rotate order every repetition, and every comparison is paired
  inside one VM. Dev `-j 4`: 6 repetitions on 2 VMs. Dev `-j 11`: 5 on 1 VM. Release `-j 4`: 3 on
  1 VM. Cold `omp`: 4 paired all and none repetitions on 2 VMs, 2 paired hot and hot-wasm
  repetitions on a third. WebAssembly probe: 10 repetitions per language on 2 VMs. Tables give the
  median, then the minimum and maximum.
- **Errors and proof of work.** A build that exits non-zero stops the run. All 390 downloads matched
  their pinned SHA-256 and size. Every native and WebAssembly parse compared its tree with the other
  (`to_sexp`). Symbol counts prove which grammars each binary linked.

## Results

### Cold build: grammars are half of `omp-ast` and 6 % of `omp`

Cold `cargo build -p omp-ast`, dev profile. MEASURED.

| Variant | `-j 4` wall s | `-j 4` CPU s | Delta vs none, `-j 4` | `-j 11` wall s | Delta vs none, `-j 11` |
|---|---|---|---|---|---|
| all (56) | 43.1 (38.8-43.5) | 156.6 | +21.4 s, 50 % (47-55 %) | 27.7 (26.3-28.3) | +15.3 s, 55 % |
| hot (17) | 24.3 (22.4-25.8) | 90.4 | +3.1 s, 13 % | 13.4 (13.2-14.3) | +1.0 s, 7 % |
| hot-wasm | 41.8 (38.2-42.9) | 161.0 | +20.8 s, 49 % | 27.5 (26.3-29.0) | +14.8 s, 54 % |
| none | 20.7 (19.3-22.9) | 76.1 | - | 12.5 (12.3-12.8) | - |

Cold `cargo build -p omp-ast --release` (`opt-level = 2`, `-j 4`, 3 repetitions): all 63.2 s
(58.5-68.0), hot 41.2 s, hot-wasm 69.9 s (67.4-72.3), none 34.9 s. The grammars add 28.2 s, 45 %.
MEASURED.

Cold `cargo build -p omp-app --bin omp`, dev profile, `-j 4`. MEASURED.

| Pair (same VM) | Repetition 1 | Repetition 2 |
|---|---|---|
| all against none, VM 1 | 439.8 s against 419.1 s, +20.7 s | 403.1 s against 398.4 s, +4.7 s |
| all against none, VM 2 | 382.8 s against 352.4 s, +30.4 s | 390.2 s against 341.7 s, +48.5 s |
| hot-wasm against hot, VM 3 | 365.8 s against 378.8 s, -13.0 s | 364.7 s against 352.9 s, +11.7 s |

All 56 grammars add a median 25.6 s (4.7 to 48.5 s) to the cold `omp` build, a median 6 % (1 to
12 %). Inside the `omp` build the grammars are 88 to 104 s of unit time. The WebAssembly runtime's
cost is inside the noise of two pairs. `omp` builds of all against hot were not paired in one VM.
[INFERENCE] By unit time the WebAssembly variant would save about 5 s of CPU, about 1.5 s of wall
time. The 39 long-tail grammars are 76 s of unit time in `omp`, and the runtime was 70.6 s of CPU
in `omp-ast`.

What limits these numbers:

- At `-j 4` the `omp-ast` build is bound by its four job slots. CPU time divided by four is within
  10 % of wall time in every variant. The grammars add 80.5 s of CPU, which is 20.1 s over four slots,
  against 21.4 s measured. The C compile inside the build scripts is 77.3 s of the grammars' 84.2 s
  of unit time.
- At `-j 11` the slots stop mattering and the longest build script decides. In all five
  repetitions `omp-ast` starts when the last grammar unit ends, at 24.3 to 26.7 s.
- The `omp` build is bound by its job slots too. The grammars' 88 to 104 s of unit time is 22 to
  26 s over four slots, against the 25.6 s median. The same variant in one VM varied by up to 37 s
  between repetitions (all: 439.8 s, then 403.1 s), which is the floor under these pairs. Six
  identical all-grammar `omp` builds on four VMs took 446 to 474 s (`raw/omp-unification-defect/`).
- Cargo caches every grammar crate after its first build. The cost recurs only when the cache is
  cold: a fresh clone, `cargo clean`, a `rust-toolchain.toml` bump, or a dependency change.
  [INFERENCE from Cargo's fingerprinting.] CI already caches the C compile itself. CI sets
  `RUSTC_WRAPPER=sccache` (`.github/workflows/ci.yml:323-324`), and `cc` 1.2.67 sends C compiles
  through that wrapper (`src/lib.rs:3283-3290`). OBSERVED.

### Incremental build: the grammars cost link time only

| Step | all | hot | hot-wasm | none |
|---|---|---|---|---|
| No-op `cargo build -p omp-ast` | 0.36 s | 0.38 s | 0.37 s | 0.35 s |
| Rebuild `omp-ast`, forced with `cargo clean -p omp-ast` | 1.97 s | 2.00 s | 2.03 s | 1.94 s |
| Rebuild `omp-kb`, library and binary | 1.66 s | 1.54 s | 1.82 s | 1.54 s |
| Link the dev `omp-kb` binary (`-Z time-passes` `run_linker`) | 285 ms | 234 ms | 381 ms | 226 ms |

Medians over 11 repetitions (33 for the link) on 3 VMs, dev profile. MEASURED. The dev `omp` link
took 5.20 s with all grammars and 5.09 s with none on one VM, and 4.29 s and 4.22 s on another
(6 links per variant per VM). MEASURED.

The claim that `omp2` recompiles the grammars on every build is false. An edit relinks the
binaries above `omp-ast`, and each link reads the grammar objects again. That costs about 60 ms per
small binary and about 0.1 s for `omp`. Per `omp-kb` link, the WebAssembly runtime costs more than
all 39 long-tail grammars.

One cold step in `tables.md` is an artefact of the harness. Building `omp-kb` after `omp-ast` took
26 s for hot-wasm against 14 s for the other variants. [INFERENCE] `omp-kb`'s extra dependencies
change the features of crates that wasmtime shares, so part of wasmtime builds a second time.

### Binary size: 96 MB per binary, 7.4 MB compressed

Release `omp-kb` (`opt-level = 2`, thin LTO, one codegen unit, stripped). MEASURED.

| Variant | Bytes | gzip -9 | zstd -19 | `.rodata` |
|---|---|---|---|---|
| all | 97,347,448 | 7,984,037 | 5,502,824 | 92,117,860 |
| hot | 12,990,872 | 1,873,958 | 1,471,633 | 10,917,508 |
| none | 1,224,856 | 563,100 | 487,147 | 87,107 |

- The grammars are 96.1 MB of the binary, and 92.0 MB of that is parse tables in `.rodata`. The
  hot set is 11.8 MB. The 39 long-tail grammars are 84.4 MB, 6.1 MB gzip, 4.0 MB zstd.
- The dev profile carries the same tables. The dev `omp` binary is 962.0 MB with all grammars,
  877.4 MB with the hot set and 865.6 MB with none, so the grammars are 10 % of it.
- The WebAssembly runtime, linked by a program that loads a module, adds 4.55 MB (1.85 MB gzip).
  That is `wasm_runtime_size` (5,527,736 bytes) against the same program without the WebAssembly
  calls (975,400 bytes). A binary that enables `tree-sitter/wasm` but never creates a store links
  little of it: release `omp-kb` grew by 0.55 MB.
- [INFERENCE] Each of the 111 binary targets that link `omp-ast` can carry the 96 MB in a dev or
  test build, so a full test build can hold up to about 10.7 GB of grammar tables in `target/`.
- [INFERENCE] The 319.6 MB macOS release binary (`omp-a37222b-aarch64-apple-darwin`, artifact of
  `package-macos` run 40064117249) links the same 57 grammars, so they are roughly 30 % of it. Its
  tarball is 103.6 MB, and the grammars would be about 7 % of that.

### First use of a WebAssembly grammar: 22 to 675 ms per process, plus the download

`grammar_probe` (release build) loads each of upstream's 39 modules in a fresh process with
`Engine::default()`, as upstream does (`crates/pi-ast/src/language/grammar.rs:106`). It parses one
real sample per language and compares the tree with the native grammar's tree. Samples come from
`github-linguist/linguist` at `d456527fe9`, plus one OpenZeppelin file for Solidity
(`grammar-bundling/samples.tsv`). MEASURED, 10 repetitions per language on 2 VMs.

| Step | Cost |
|---|---|
| Download one `.zst` asset with a new HTTP client | median 246 ms (210-839 ms over 390 downloads); size barely matters |
| zstd decode and SHA-256 check | under 33 ms for the largest module (18.1 MB Verilog) |
| `Engine::default()` | 0.09 ms |
| `WasmStore::new`, per store | 8.3 to 14.4 ms; upstream builds one per thread and again every 4 MiB parsed |
| `load_language` (Cranelift compile) | 3 ms (INI) to 648 ms (Fortran); 11 languages over 100 ms |
| Whole first use, read to first tree | 22 to 675 ms; median over languages 66 ms |
| Native first parse of the same sample | 0.1 to 37 ms |

Compile time does not follow module size. PowerShell (0.99 MB) takes 545 ms and Verilog (18.1 MB)
173 ms. The slowest ten are Fortran 648, PowerShell 545, Swift 493, Julia 428, Scala 191, OCaml 177,
Kotlin 175, Verilog 173, Objective-C 158 and TLA+ 136 ms. Nothing caches the compiled module on
disk, so every new process pays the compile again on first use. Upstream compiles once per process
(`grammar.rs:110` and `WasmGrammar::load`).

Swift's WebAssembly tree differed from the native tree in all 10 runs. Upstream built that module
from `tree-sitter-swift` 0.7.3, and `omp2` pins 0.7.4. A borrowed module set drifts from `omp2`'s
pins unless `omp2` builds its own modules.

### Steady-state parsing: WebAssembly is 1.10 to 2.45 times slower and grows its store

One parser parses the same sample again and again, native and WebAssembly in interleaved blocks. The
ratio is the median WebAssembly parse over the median native parse. MEASURED, 2 VMs, 25 to 2,000
parses per side.

- WebAssembly is slower for every language: median 1.74 times, from 1.10 (Verilog) to 2.45 (Vue).
  The two VMs agreed within 0.15 on every ratio. Only lexing and external scanners run in
  WebAssembly, and tree-sitter keeps the parse tables native (`grammar.rs:13`). [INFERENCE] That is
  why small grammars with busy lexers, such as Vue, Astro and Svelte, lose the most.
- A WebAssembly parser's store grew by 0 to 59 bytes per byte parsed, median 14.5. CMake reached 59,
  HCL 55, Haskell 43, Dart 39 and Kotlin 38 bytes per byte. The native parsers grew by 0 KB. With
  upstream's 4 MiB budget (`grammar.rs:123`), one thread can hold up to about 240 MiB before its
  store is replaced. [INFERENCE: 59 bytes per byte times 4 MiB.] Upstream's issue #16067 is this
  growth without a budget: a 40 GiB reallocation aborted the process (`grammar.rs:112-122`).
- Every WebAssembly tree but Swift's matched the native tree exactly.

## Options compared

| Option | Cold `omp-ast` build, dev `-j 4` | Release bytes per binary | Run-time cost | New machinery |
|---|---|---|---|---|
| Keep all 56 compiled in | 43.1 s | +96.1 MB, +7.4 MB gzip | none | none |
| Upstream: 17 compiled in, 39 as WebAssembly | 41.8 s | +11.8 MB and a 4.6 MB runtime | 22-675 ms per language per process, 246 ms download, parsing 1.74 times slower, store growth | wasmtime, loader, store budget, download and verify and cache, offline path, a module build pipeline or a third-party release |
| Feature-gated sets: all for release, 17 for development and test builds | 24.3 s where a build opts in | unchanged for release, +11.8 MB in opted-in builds | none | Cargo features threaded through the 11 dependent crates, long-tail tests behind the feature |
| Smaller default set, long tail opt-in at build time | 24.3 s | +11.8 MB | 39 languages missing unless the user rebuilds | the same features, plus a second build for users who need them |
| Native grammar libraries downloaded and loaded at run time | about 24.3 s [INFERENCE] | +11.8 MB and a loader | download and `dlopen`, native parse speed [INFERENCE] | a build per platform, macOS signing of downloaded code, unsandboxed native code from the network |
| A compile cache, product unchanged | unchanged when cold, C objects reused when the cache is warm [INFERENCE] | +96.1 MB | none | already in CI; developers opt in with `RUSTC_WRAPPER=sccache` |

My view of the table:

- **Keep compiled-in** wins on everything but bytes. The bytes matter for disk and download, not for
  speed, and they compress 13 to 1.
- **Upstream's model** buys the bytes back. But `omp2` has no WebAssembly runtime, and adding one
  costs about the build time that the long-tail grammars cost. `omp2` would carry the runtime for
  grammars alone.
- **Feature-gated sets** are the best lever if build time or `target/` size ever becomes the
  problem. The harness branch shows they compile, they change nothing for users, and they cut the
  `omp-ast` cold build by 44 %. They are not worth their plumbing today, because the cold `omp`
  build moves by a median 25.6 s of about 400 s.
- **A smaller default set** is a product decision. Without usage data, it removes 39 languages from
  structural search, `read` summaries and the knowledge base for every user to save bytes.
- **Native libraries at run time** keep native speed. [INFERENCE] They keep every cost of the
  WebAssembly model except the runtime, and they add per-platform builds and native code from the
  network. I would not pick them over the WebAssembly model.

## What this study did not measure

- **macOS and Windows builds.** No compile ran on the Mac: it is the CI runner, it was busy, and it
  has 16 GB and swaps. The `-j 11` Linux runs stand in for a 10-core laptop.
- **A release `omp` build with and without grammars.** Not attempted. `scripts/remote-verify.sh`
  caps a run at 60 minutes, and release `omp-kb` links the same grammar objects.
- **The cost of the long tail inside the `omp` build.** All against hot was not paired in one VM.
  The estimate above is [INFERENCE] from unit time.
- **Download time on home or office networks.** Only datacenter downloads were timed. A planned
  timing from the Mac was dropped because the Mac was busy with CI.
- **The native-library loader.** No prototype. Its numbers in the table are [INFERENCE].
- **A compiled-module cache for wasmtime.** wasmtime can cache compiled modules on disk when its
  `cache` feature is on and the `Engine` is configured for it. `tree-sitter` 0.25.10 enables only
  `cranelift` and `gc-drc` on wasmtime (its `Cargo.toml`), and the probe used `Engine::default()`
  as upstream does.
- **Resident memory of compiled-in grammars.** [INFERENCE] Parse tables are read-only data that the
  OS pages in on use.
- **Language usage in the field.** `omp2` records none.

## Reproduce

The harness lives on two branches that are never merged:

- `research/grammar-bundling-harness` at `03cf2f9768`: the grammar features, the probes
  (`crates/ast/examples/grammar_probe.rs`, `wasm_runtime_size.rs` and `native_runtime_size.rs`) and
  `scripts/grammar-study.sh`.
- `research/grammar-bundling-harness-omp` at `b04b268acd`: the same harness plus the `omp` phase.
  It turns off `omp-ast`'s default features, so the workspace's own feature unification can pick
  the grammar set.

To rerun:

1. Fetch the samples listed in `grammar-bundling/samples.tsv` into
   `.grammar-study/samples/<language>.txt`. Leave them untracked: `bb` mirrors untracked files to
   the runner.
2. Run each phase with
   `OMP_REMOTE_KEYCHAIN_SERVICE=<service> just remote grammar-study <phase> <reps> [jobs] [variants]`.
   The phases are `check`, `dev`, `release`, `probe`, `bytes` and `omp`. Write `omp` variants
   as `all+none`, because `bb` splits `--env` values on commas.
3. Save the `GS` lines of each log as `grammar-bundling/raw/<run>.tsv`, then run
   `python3 docs/research/grammar-bundling/analyze.py > docs/research/grammar-bundling/tables.md`.

Two harness defects surfaced during the runs. Both are fixed on the branches:

- rustfmt split a long format string inside a tab escape, which broke every `first` line of the
  first probe run in two. Those lines were joined again before analysis (`raw/probe-r1.tsv`).
- The first `omp` runs left `omp-ast`'s default features on. Workspace feature unification builds
  every member with its defaults, so all seven of those builds linked all 57 grammars. They are kept
  in `raw/omp-unification-defect/` only as a measure of VM-to-VM noise, and no table uses them.
