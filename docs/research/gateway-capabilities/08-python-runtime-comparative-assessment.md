# Python runtime comparative assessment

> Part of the [OMP2 gateway-capabilities research](README.md). Deliverable 8. Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Evidence: [Python runtime audit](evidence/omp2-python-runtime.md), [TSV](evidence/omp2-python-runtime.tsv), [diagram 12](diagrams/12-embedded-vs-standalone-python-workers.md), measurements [60](evidence/measurements/60-python-footprint.md), [61](evidence/measurements/61-python-subprocess-startup.md), [62](evidence/measurements/62-jsonl-roundtrip.md). Method: [method](evidence/01-method-and-dimension-ids.md).

## 1. Is embedded CPython still justified?

**Yes, as the default for extension hosts, named workers and Eval, on the evidence available. No evidence supports replacing it, so nothing here proposes changing ADR 0036.**

The owner's question assumes that isolation and embedding are the same lever. They are not. Isolation (kill, crash containment, hot reload) already comes from the child process (`crates/envd/src/exthost/spawn.rs:543 spawn`, `crates/envd/src/eval/process.rs:569 spawn`), and a standalone CPython would be just as killable. What embedding buys is different:

1. **No installed Python.** Eval and extensions work on hosts with none (ADR 0036 consequence 1). The standalone path exists only as an opt-in for Eval (`crates/envd/src/eval/process.rs:136 OMP_PYTHON_INTERPRETER`).
2. **A native `_omp` module in the interpreter** (`crates/py/src/bindings.rs:4349 pymodule`): typed values, a Rust Environment DATA client with the interpreter detached while blocking (`crates/py/src/bindings.rs:1546`), and thread interruption (`crates/py/src/interrupt.rs:26 interrupt`). A standalone interpreter would need a pure-Python re-implementation or a wire port; Eval already has one for its protocol (`crates/envd/src/eval/external_runner.py:1`) but extension hosts do not.
3. **One executable to sandbox and ship.** On Linux the sandboxed tier reads only the executable, the site tree and the entry file, with no network (`crates/envd/src/exthost/spawn.rs:553 sandbox`). On macOS the allow-list already widens to the loaded dylib directories and package-manager prefixes (`crates/envd/src/exthost/spawn.rs:472 allow_loaded_runtime_images`), so this advantage is smaller than ADR prose implies.
4. **Free-threaded workers** serve concurrent calls on threads (ADR 0036 consequence 3); a warm named worker avoids a process per call.

What embedding costs, with the unmeasured parts labelled:

- Interpreter build chain and per-target notices; two supported targets only (`crates/py/src/lib.rs:100 compile_error`); local vendor trees of 166 MB (debug flavour) and 182 MB (release flavour), a build input not a shipped size ([60](evidence/measurements/60-python-footprint.md)).
- Narrower wheel compatibility: the embedded interpreter accepts only `cp314t`, `abi3t` and pure wheels (`docs/py/14-deploy.md:105`). This is the one place a standalone interpreter is strictly better, and Eval already offers it.
- Binary size delta, child startup and RSS: `not measured` (PY-02).

The latency argument cuts neither way yet. A process per session is paid in both designs. The standalone floor on this loaded laptop was a median of about 27 ms for the vendored 3.14t and about 49 ms for the system 3.15 (30 runs, [61](evidence/measurements/61-python-subprocess-startup.md)); the embedded child was not measured.

## 2. Comparison

Legend: **M** measured here (file number), **R** read in code or docs, **NM** not measured (plan in section 4), **D** documentation-level expectation, not verified here.

| Criterion | Embedded CPython (PyO3, current) | Standalone CPython subprocess | RustPython | Native Rust executable extension | Go executable adapter | Wasm component |
|---|---|---|---|---|---|---|
| Binary size | NM (PY-02); vendored install trees 166 MB debug and 182 MB release are build inputs (M 60) | none added to `omp`; separate interpreter artifact to ship | D, unverified | none added to `omp` | none | NM: wasmtime plus runtime would be added (PY-08) |
| Startup latency | NM (PY-02) | M 61: median 27 to 49 ms per spawn on a loaded 10-core Mac; first exec of the vendored binary took 627 ms once (macOS first-run check, one sample) | D | not applicable (no such extension kind) | not applicable | NM |
| Memory footprint | NM (PY-02) | NM | D | NM | NM | NM |
| Package compatibility | only `cp314t`, `abi3t`, pure wheels (R `docs/py/14-deploy.md:105`) | any CPython 3.10 or newer for Eval (R `crates/envd/src/eval/process.rs:1865 external_python_version_supported`) | D: native wheel support unverified, PY-06 | not Python | not Python | D: native wheels not generally loadable, PY-08 |
| Callback overhead | NM (PY-03); typed in-process `_omp` calls with `py.detach` (R) | M 62: pipe plus JSON echo, median 14 to 17 us per call (floor only) | NM | message passing | message passing | WIT canonical ABI calls, NM |
| Distribution | one executable re-entered with hidden argv (R `crates/app/src/main.rs:60 main`); two targets | needs an interpreter on the host or a pinned one beside the binary | D | one more binary per extension | one more binary per extension | `.wasm` file plus host runtime |
| Isolation | child process plus sandbox tier (R `crates/envd/src/exthost/spawn.rs:543 spawn`) | same child plus sandbox; larger read allow-list (not tested) | in-process unless hosted in a child | process | process | linear-memory sandbox; kill by epoch or fuel trap, not process kill |
| Maintainability | interpreter build chain, `-export_dynamic`, one runtime per process (R `AGENTS.md:688`) | second protocol peer already exists for Eval (`external_runner.py`); none for ext hosts | new embedding and compatibility triage | no SDK exists | no adapters exist | new WIT contract and SDK |

## 3. Facts about the other options

- **Standalone CPython.** Implemented only for Eval. It uses the same pipes, environment sanitation, process group and sandbox wrapper as the embedded child (`crates/envd/src/eval/process.rs:586 command_for`). Extension hosts and workers cannot use it.
- **RustPython.** Documentation only. Not in `Cargo.lock`, not read, no clone. All expectations about it are unverified (PY-06).
- **Native Rust executable extensions.** No such extension kind. Non-Python programs integrate through MCP stdio or exec (`crates/envd/src/mcp/transport/stdio.rs:1`; ADR 0046 line 52), which is the existing answer (PY-07).
- **Go adapters.** None exist in the tree (PY-09).
- **Wasm components.** None in the tree. Reference facts: wasmtime exposes fuel and epoch interruption (`bytecodealliance/wasmtime@b738a89:crates/wasmtime/src/config.rs`) and `ResourceLimiter`; wit-bindgen targets guest languages Rust, C, C++, C# and Go and refers to other tooling for the rest (`bytecodealliance/wit-bindgen@e3b53be:README.md`). The Canonical ABI is not the C ABI, but it does not remove the need for a kill boundary that ADR 0011 requires.

## 4. Not measured, with plans

| Item | Status | Plan |
|---|---|---|
| Embedded child cold and warm startup | not measured | PY-02: stub parent sends Init, time to Ready frame, 30 runs, `uptime` recorded |
| Binary size delta of `omp-py` | not measured | PY-02: `just build-release` with and without the interpreter link, `stat` both |
| RSS of embedded versus standalone child | not measured | PY-02: `ps -o rss` after Ready, both |
| PyO3 callback overhead | not measured | PY-03: `criterion` benches plus Python timing loop |
| Wheel coverage for `cp314t` | not measured | sample real `$OMP_PY_SITE` trees or resolve a package list with `uv` against both interpreters |
| External runner parity | not verified | PY-04 contract tests on both interpreters |

## 5. Capability cards

### PY-01 Keep embedded free-threaded CPython as the default runtime of extension hosts, workers and Eval
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `crates/py/src/lib.rs:179 init`; `crates/envd/src/exthost/spawn.rs:746 run_ext_host_entry`; `crates/envd/src/worker_child.rs:55 run_worker_child_entry`; `crates/envd/src/eval/process.rs:1550 run_eval_child_entry`; ADR 0036 decisions 1 and 5 (Implemented in code, STATUS says Partially implemented because `@remote` placement is unproved).
**Actual missing capability:** None for the embedding itself. The change that would be needed to drop it (a replacement runtime) has no evidence behind it, see PY-05, PY-06, PY-08.
**Alternatives considered:** Standalone CPython for everything (PY-05), RustPython (PY-06), Wasm components (PY-08).
**Expected benefit:** Distribution (one executable, no installed Python, ADR 0036 consequence 1), security (small read allow-list on Linux, no network for the sandboxed tier), reliability (`_omp` native module and the interpreter always match the binary).
**Implementation complexity:** low: nothing to build.
**Security implications:** Child already isolates the interpreter from the agent process. Embedding does not add isolation by itself; isolation comes from the process and sandbox (see the answer in section 1).
**Maintenance cost:** Carries the interpreter build chain: `PYO3_CONFIG_FILE`, python-build-standalone fetch, lld for release, per-target notices, `-export_dynamic` in every consumer (`AGENTS.md:688`). Two targets only.
**Performance measurement strategy:** not applicable (decision retained); cost measurements are PY-02 and PY-03.
**Acceptance criteria:** The embedded children keep passing `crates/py/tests/*` and `crates/envd/src/eval/tests.rs`; no code path in the engine process initializes Python outside tests and `omp setup python`.
**Follow-up:** Record in ADR 0036 status that `omp setup python` boots Python in the CLI process (PY-10).

### PY-02 Measure embedded child startup, resident memory and the binary size delta of omp-py
**Decision class:** Requires benchmarking
**Existing implementation evidence:** `crates/envd/src/eval/process.rs:569 spawn` has a `tracing` span `eval_child_spawn`; `crates/py/src/lib.rs:179 init` has spans `python_inittab`, `python_stdlib_install`, `python_runtime_boot` (debug level). No benchmark or recorded number for these was found in the files read for this audit (a full-tree search for one was not done). Standalone floor measured in [61](evidence/measurements/61-python-subprocess-startup.md).
**Actual missing capability:** Startup latency (cold and warm) of `omp __omp-eval-child` and `omp __omp-ext-host` to the Ready frame, RSS after Ready, and bytes added by linking `omp-py` plus the frozen stdlib. All `not measured`; no `omp` binary was built in this pass and the only built binary found is a 735 MB debug e2e host.
**Alternatives considered:** Reading the tracing spans only (misses process spawn and exec cost); comparing against the 27 to 50 ms standalone floor only (different work, loaded machine).
**Expected benefit:** Turns the embedded-versus-standalone argument into numbers: latency, memory.
**Implementation complexity:** low to medium: a release build with and without the `omp-py` dependency, plus a small harness around the hidden argv roles.
**Security implications:** none beyond running the existing child entries; run on an isolated checkout, do not run untrusted extensions.
**Maintenance cost:** low: one script and a result file.
**Performance measurement strategy:** On a quiet machine (record `uptime`): `just build-release` twice (with and without omp-py in the link, or `size` of the `omp` binary versus a build of the same revision where `crates/py` is stubbed); `stat` both. For startup: 30 runs of spawning the hidden role with a stub parent that sends Init and measures time to the Ready frame, cold (after `purge`, needs root) and warm; RSS via `ps -o rss` after Ready. Compare against standalone `python3.14t -c pass` from the same vendored tree.
**Acceptance criteria:** A measurement file under `evidence/measurements/` with commands, environment, raw output, number of runs; medians and p95 for startup, RSS in MB, binary delta in MB.
**Follow-up:** If the embedded child is not at least as fast as the standalone floor plus the cost of installing the frozen table, open PY-05 evidence work.

### PY-03 Measure PyO3 callback overhead on the _omp boundary
**Decision class:** Requires benchmarking
**Existing implementation evidence:** `crates/py/src/bindings.rs:4349 pymodule` (`_omp`, `gil_used = false`); blocking calls detach with `py.detach` (`crates/py/src/bindings.rs:1546`) and run on a private Tokio runtime (`crates/py/src/bindings.rs:144 ASYNC_RUNTIME`). The comparable standalone cost floor for a pipe plus JSON call is [62](evidence/measurements/62-jsonl-roundtrip.md): median 14 us (system python3 3.15.0) and 17 us (vendored 3.14t), echo child, loaded machine.
**Actual missing capability:** Per-call cost of Python to Rust (`omp.env` read, `Duration` construction, `Secret` use, stream `__next__`) and Rust to Python (`engine.attach`), under 1, 8 and 32 concurrent Python threads on the free-threaded build. `not measured`.
**Alternatives considered:** Counting cost per call from tracing spans (too coarse, adds overhead).
**Expected benefit:** Shows whether the native `_omp` boundary beats a JSON message to a standalone child by a margin that matters (ADR 0036 consequence 3 says extensions are under 1 percent of session time).
**Implementation complexity:** medium: a Rust bench crate or `criterion` benches calling `#[pyfunction]` entries through `Engine::attach`, and a Python-side loop with `time.perf_counter_ns`.
**Security implications:** none: benchmark runs on local fixtures.
**Maintenance cost:** low to medium.
**Performance measurement strategy:** Rust `criterion` benchmark for attach plus a no-op `_omp` function, and a Python timing loop for typed value constructors; 5 runs of 100000 calls each, report median and p95, record `uptime`. Pair with the 62 harness replaced by the real `external_runner.py` for the standalone side.
**Acceptance criteria:** Result file with both sides under the same load, number of runs, and an explicit statement of what the callback does not include (I/O, serde of large payloads).
**Follow-up:** If the callback gap is under the pipe floor, the `_omp` module is a convenience, not a performance reason; decide in PY-05.

### PY-04 Standalone CPython subprocess for Eval (existing opt-in)
**Decision class:** Already implemented: reuse
**Existing implementation evidence:** `crates/envd/src/eval/process.rs:74 EMBEDDED_INTERPRETER`; `OMP_PYTHON_INTERPRETER` at `crates/envd/src/eval/process.rs:136`; version gate `crates/envd/src/eval/process.rs:1865 external_python_version_supported`; staged runner `crates/envd/src/eval/process.rs:1829 stage_external_runner`; pure-Python protocol peer `crates/envd/src/eval/external_runner.py:1`. Maturity: Implemented (reachable from `ProcessEvalExec::production`), parity with the embedded surface not verified.
**Actual missing capability:** Nothing to build. Gaps: no documented parity table between `external_runner.py` and the embedded prelude, and no statement in ADR 0036 or STATUS that the mode exists.
**Alternatives considered:** Making external the default for Eval (breaks ADR 0036 decision 1 and 'works with no Python installed').
**Expected benefit:** Package compatibility: an external interpreter can be any CPython 3.10 or newer, so wheels that have no `cp314t` build are usable for Eval; the embedded one accepts only `cp314t`, `abi3t` and pure wheels (`docs/py/14-deploy.md:105`).
**Implementation complexity:** low: already built.
**Security implications:** The external interpreter runs the user's Python with the same sanitized environment and optional sandbox wrapper (`crates/envd/src/eval/process.rs:586 command_for`); it adds dependence on a host interpreter, which ADR 0036 decision 1 forbids as a default.
**Maintenance cost:** One more protocol peer to keep in step (`external_runner.py` versus the Rust child).
**Performance measurement strategy:** Run the same Eval contract tests (`crates/envd/src/eval/tests.rs`) against both interpreters and report pass counts; time cell round trips with 62 style harness through the real runner.
**Acceptance criteria:** A parity table lists every prelude helper as supported, different or missing under the external runner, with a test per row.
**Follow-up:** Pair with PY-10.

### PY-05 Standalone CPython as the runtime for extension hosts and workers
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** Extension hosts and workers re-enter the same binary only (`crates/envd/src/exthost/spawn.rs:584 command`, `crates/envd/src/worker_child.rs:84 omp_py`); `omp._host` depends on the native `_omp` module (`crates/py/src/bindings.rs:4349 pymodule`). ADR 0036 decision 1 forbids a host-installed interpreter as the dependency.
**Actual missing capability:** Missing evidence that would justify reopening: (1) embedded child startup and RSS versus standalone on the same machine (PY-02); (2) callback gap versus message passing (PY-03); (3) a pure-Python implementation or a wire port of the `_omp` surface (typed `Secret`, `EnvPath`, Environment DATA client, interrupt), with an effort estimate; (4) the effective sandbox read allow-list with a standalone prefix and stdlib on macOS and Linux; (5) proof that the extension ecosystem needs wheels that have no `cp314t` build, with counts from `$OMP_PY_SITE` of real users.
**Alternatives considered:** Keep embedded (chosen, PY-01); ship a pinned standalone CPython beside the binary, which removes the 'no installed Python' advantage only partly and adds an artifact to sign.
**Expected benefit:** Possible wider wheel coverage, smaller `omp` binary, simpler build (no PYO3 config, no export list). All unproven here.
**Implementation complexity:** high: a second host implementation, `_omp` shim, signing and updating the interpreter, new sandbox allow-lists.
**Security implications:** Larger read allow-list in the sandboxed tier; a second distribution artifact to verify (`crates/ext/src/trust.rs` covers extensions, not interpreters).
**Maintenance cost:** Two runtimes to support during migration; protocol parity risk.
**Performance measurement strategy:** Run PY-02 and PY-03; add a standalone host spike behind a flag in a scratch branch only after (1) to (5) exist.
**Acceptance criteria:** An evidence bundle covering items (1) to (5) is attached to a draft ADR superseding part of ADR 0036 decision 1; until then no code change.
**Follow-up:** Do not start before PY-02 and PY-03 report.

### PY-06 RustPython as an embedded alternative
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** None in tree: `Cargo.lock` has no `rustpython` entry (`02-verified-current-state.md` section 7). RustPython was not examined beyond that in this pass; no reference clone exists and no network fetch was made.
**Actual missing capability:** Missing evidence: (1) whether RustPython can import the pinned pure-Python packages in `crates/py/requirements.txt` and the repo modules under `crates/py/python`; (2) whether it can load native wheels at all (ADR 0036 decision 4 relies on `dlopen` of `cp314t` wheels into the embedded interpreter); (3) a threading story equal to free-threaded CPython for warm workers (ADR 0036 consequence 3); (4) startup, size and memory numbers against PY-02. Statements about RustPython in this document are documentation-level expectations, not verified here.
**Alternatives considered:** Stay on CPython (chosen).
**Expected benefit:** Possibly a Rust-native build with no C interpreter build chain. Unproven.
**Implementation complexity:** high: new embedding, new bindings for the `_omp` surface, compatibility triage.
**Security implications:** New large dependency in the trusted computing base; interpreter-level sandbox escapes are untested.
**Maintenance cost:** A second interpreter's compatibility matrix.
**Performance measurement strategy:** Compatibility run of the repo's frozen modules and `crates/py/tests/*` equivalents; size and startup via PY-02.
**Acceptance criteria:** All four evidence items present and a draft ADR accepted; nothing earlier.
**Follow-up:** Do not start; documented only.

### PY-07 Non-Python tools (Rust, Go, any language) through existing MCP stdio instead of a new plugin runtime
**Decision class:** Suitable as an external integration
**Existing implementation evidence:** MCP stdio transport with process-group termination: `crates/envd/src/mcp/transport/stdio.rs:1` (ADR 0011 cites it as `mcp/stdio.rs`; the file is under `transport/`); ADR 0036 decision 2 forbids a second extension language; ADR 0046 line 52 keeps foreign executables unloaded. The MCP stdio and ADR citations hold. An independent spot check found only `PluginLaunchKind::Hook` and `McpServer` in `crates/ext`; that is not a full survey, so "no native Rust executable extension kind and no Go adapter exist" rests on the extension audit's search (see evidence file section 5) and was not re-confirmed.
**Actual missing capability:** Nothing new. If a Rust or Go program must be offered as a tool it can be an MCP stdio server or an `exec` target; it is not an extension in the ADR 0036 sense.
**Alternatives considered:** A native Rust executable extension protocol (needs ADR 0036 decision 2 superseded), `abi_stable` or dynamic libraries (`Cargo.lock` has none).
**Expected benefit:** Language freedom without a new runtime; reuse of the existing kill boundary and credential handling.
**Implementation complexity:** low: documentation of the supported path.
**Security implications:** Same as MCP stdio today: unsandboxed unless wrapped by the exec sandbox; credentials only through declared grants.
**Maintenance cost:** None added.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** The extension guide states that non-Python tools integrate through MCP stdio or exec and never load into the host.
**Follow-up:** Owned by the extension audit; no work here beyond a doc pointer.

### PY-08 WebAssembly components as an extension runtime
**Decision class:** Requires ADR reconsideration
**Existing implementation evidence:** None in tree: no `wasmtime`, `wasmer` or `extism` in `Cargo.lock`. Reference facts: wasmtime has fuel (`bytecodealliance/wasmtime@b738a89:crates/wasmtime/src/config.rs` `consume_fuel`) and epoch interruption (`epoch_interruption`, same file) and a `ResourceLimiter`; wit-bindgen is focused on guest languages Rust, C, C++, C# and Go and points to separate tooling for others (`bytecodealliance/wit-bindgen@e3b53be:README.md`). The Wasm Canonical ABI is a distinct thing from the C ABI and does not by itself solve dynamic loading of OMP extensions.
**Actual missing capability:** Missing evidence: (1) a WIT world for the `omp.*` surface and cost of mapping `Secret`, `EnvPath`, streams and DATA; (2) Python on Wasm that can import the frozen pure packages and the wheels extension authors use (native wheels are not loadable there in general); (3) kill semantics: epoch or fuel traps stop guest code at instruction boundaries, but ADR 0011 asks for a process-level kill boundary and the host would then share the engine's address space, unless hosted in a child anyway; (4) startup, memory and binary size numbers for a Wasm host (wasmtime plus component runtime added to the binary).
**Alternatives considered:** Keep Python in child processes; keep Wasm out (ADR 0036 decision 2).
**Expected benefit:** Capability-style sandboxing without a process per extension. Unproven for this codebase.
**Implementation complexity:** high: new runtime, WIT contract, SDK in at least one guest language.
**Security implications:** Smaller syscall surface by construction, but a new large dependency (cranelift, wasmtime) enters the trusted computing base and the extension contract changes.
**Maintenance cost:** A second extension runtime; ABI versioning of WIT worlds.
**Performance measurement strategy:** Spike measuring wasmtime binary size delta and cold instantiate time of a hello component; only after evidence items 1 to 3.
**Acceptance criteria:** Draft ADR supersedes ADR 0036 decision 2 with the four evidence items attached.
**Follow-up:** Do not start; documented only.

### PY-09 Go executable adapters as Python-runtime alternatives
**Decision class:** Unnecessary or redundant
**Existing implementation evidence:** None found in OMP2. Searched: `git ls-files '*.go' go.mod`, grep for `go adapter`, `go executable`, `go binary` in `crates`, `docs`, `extensions`, `AGENTS.md`. Only a Go test fixture (`fixtures/sarif/sources/golangci/outcomes.go`) and a remark that a Maven or Go build adapter would be a new adapter, not a backend change (`crates/exec/README.md:38`).
**Actual missing capability:** Nothing to compare: there is no adapter set to evaluate. The only Go-in-the-loop precedent found is v1's external `ovnfs` daemon (`docs/research/subagent-isolation.md`, upstream pi), which is a daemon, not an extension adapter.
**Alternatives considered:** Treat Go programs as MCP or exec targets (PY-07).
**Expected benefit:** none (no new capability).
**Implementation complexity:** not applicable.
**Security implications:** not applicable.
**Maintenance cost:** none.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** Re-open only if a Go adapter appears in the tree.

### PY-10 Record the dual-mode Eval interpreter and the per-child role table in ADR 0036 status and docs
**Decision class:** Partially implemented: extend
**Existing implementation evidence:** The behaviour exists (`crates/envd/src/eval/process.rs:136 OMP_PYTHON_INTERPRETER`, `crates/app/src/setup_cmd.rs:44 python`, `crates/app/src/main.rs:60 main`); ADR 0036 text and `AGENTS.md:688` say 'ONE embedded runtime' and 'boots in-process' without these qualifications; the path `crates/envd/src/worker` is now `worker.rs`, `worker_child.rs`, `worker_pool.rs`, `worker_process.rs`.
**Actual missing capability:** Documentation: a status note listing the four hidden roles, the three wire formats, the external Eval option and the `omp setup python` CLI boot.
**Alternatives considered:** Leaving the drift as it is.
**Expected benefit:** Reliability of the record; avoids a wrong premise in later reviews.
**Implementation complexity:** low.
**Security implications:** none.
**Maintenance cost:** low.
**Performance measurement strategy:** not applicable.
**Acceptance criteria:** ADR 0036 status section and `docs/py/00-overview.md` state the role table; `scripts/` docs checks pass.
**Follow-up:** Owner decides whether this is a status edit or an amendment.

## 6. Draft ADR needed?

**No new ADR is needed now.** No evidence supports replacing ADR 0036. Two small points need the owner's choice between a status edit and an amendment: the opt-in external Eval interpreter and the `omp setup python` CLI boot are not in ADR 0036 text (PY-10). A superseding ADR becomes appropriate only if PY-02, PY-03 and the PY-05 evidence items show a gap; the draft would then be scoped to ADR 0036 decision 1 (and decision 2 for PY-08), and it would carry that evidence.

Back to the [index](README.md).
