# OMP2 audit: Python runtime

> Part of the [OMP2 gateway-capabilities research](../README.md). Deliverable 8 (evidence). Audited commit `85d219ae7f` (`origin/omp2`). Research proposal: nothing here is implemented.

Scope: how Python runs in OMP2 at `85d219ae7f`. Method and labels: [method](01-method-and-dimension-ids.md). Paths are relative to the repository root; the audited tree is the `origin/omp2` worktree and the primary checkout (same commit). The interpreter itself lives under git-ignored `/vendor`, so only the pyo3 configs and sizes were read there.

## 1. One binary, four hidden child roles

`crates/app/src/main.rs:60 main` inspects `argv[1]` before normal startup and re-enters the same executable in one of these roles (lines 65-130): sandbox child (`HIDDEN_CHILD_ARG`), eval child (`EVAL_CHILD_ARG`), shell child, named Python worker (`PY_WORKER_ARG`), extension host (`EXT_HOST_ARG`). Embedded Python is booted only in three of them plus the `omp setup python` diagnostic.

| Role | Hidden arg | Entry | Boots embedded CPython | Wire format to parent |
|---|---|---|---|---|
| Eval session child | `__omp-eval-child` | `crates/envd/src/eval/process.rs:1550 run_eval_child_entry` | yes (line 1591) unless an external interpreter is configured | newline-delimited JSON on stdin/stdout, 64 MiB frame cap (`crates/envd/src/eval/process.rs:1804 write_frame`) |
| Extension host | `__omp-ext-host` | `crates/envd/src/exthost/spawn.rs:746 run_ext_host_entry` | yes (line 763), then `omp._host.bootstrap().run_forever()` | CONTROL on inherited fd 3 (`UnixStream::pair`), u32 big-endian length then JSON (`crates/envd/src/exthost/control.rs:3780 from_be_bytes`), plus a separate DATA unix socket |
| Named worker | `__omp-py-worker` | `crates/envd/src/worker_child.rs:55 run_worker_child_entry` | yes (line 84), then `omp._worker.run_forever` | varint length-prefixed protobuf `WorkerFrame` on stdio, Ping/Pong health loop (`crates/envd/src/worker_child.rs:95 health_loop`) |
| `omp setup python` | none (CLI) | `crates/app/src/setup_cmd.rs:44 python` | yes, in the CLI process, one-shot diagnostic | none |

So three different framings carry Python child traffic today (JSON lines, u32+JSON, varint+protobuf). That is a maintenance fact for deliverable 6 and the IPC audit, not a runtime defect.

## 2. The embedding crate

- `crates/py/src/lib.rs:1` doc: statically linked free-threaded 3.14t, stdlib frozen in memory, `PyImport_FrozenModules` table installed by `crates/py/src/lib.rs:293 install_frozen_modules` from `STDLIB_BLOB` (`crates/py/src/lib.rs:58`) and `OMP_MODULES_BLOB` (line 64).
- One runtime per process: `crates/py/src/lib.rs:108 INITIALIZED` swap in `crates/py/src/lib.rs:179 init`. A second interpreter means a second process.
- Isolated config: `crates/py/src/lib.rs:394 init_python` uses `PyConfig_InitIsolatedConfig`, `site_import = 0`, `write_bytecode = 0`, exactly one search path (site-packages), `use_frozen_modules = 1`.
- Targets: `crates/py/src/lib.rs:100` `compile_error!` for anything except macOS and Linux; the tracked notices are `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu` only (lines 89-98).
- Native module: `crates/py/src/bindings.rs:4349 pymodule` declares `_omp` with `gil_used = false`, registered before init through `append_to_inittab!` (`crates/py/src/bindings.rs:4345 register`). It exports typed values (`Duration`, `Secret`, `EnvPath`, `BlobRef`, `Principal`), the Environment client and `_interrupt`.
- Rust DATA client inside the Python child: `crates/py/src/bindings.rs:144 ASYNC_RUNTIME` is a private multi-thread Tokio runtime named `omp-py-data`; blocking iterators release the interpreter with `py.detach` (`crates/py/src/bindings.rs:1546`). This is the real callback boundary: Python code calls Rust, Rust blocks on I/O with the interpreter detached.
- Async interruption: `crates/py/src/interrupt.rs:26 interrupt` raises `KeyboardInterrupt` in one live thread through `PyThreadState_SetAsyncExc`; this is cooperative inside the child, with the process kill as the hard stop.
- Native wheels: the final binary must export the CPython C API (`crates/py/link/`, `-export_dynamic`, `AGENTS.md:704-715`); site-packages is the only real path and takes only `cp314t`, `abi3t` or `none` wheels (`docs/py/14-deploy.md:105`).
- `crates/envd/src/tools/registry.rs:2464 python_engine` is `#[cfg(test)]` only, so the engine library never boots Python in its own process outside tests.

## 3. Embedded versus external interpreter inside Eval (code departs from ADR text)

ADR 0036 decision 1 says the harness "NEVER depends on a host-installed interpreter". The default still holds, but Eval has an opt-in second path:

- `crates/envd/src/eval/process.rs:74 EMBEDDED_INTERPRETER` is the default; `crates/envd/src/eval/process.rs:136 OMP_PYTHON_INTERPRETER` or a configured interpreter selects an external one (`resolve_configured_python`, line 1850).
- For an external interpreter the parent runs `<python> -u <staged omp-eval-runner-ULID.py>` after a `--version` probe that requires Python 3.10 or newer (`crates/envd/src/eval/process.rs:1865 external_python_version_supported`, `crates/envd/src/eval/process.rs:1829 stage_external_runner`).
- `crates/envd/src/eval/external_runner.py:1` reimplements the child side of the JSON-lines protocol in pure Python: duplicates fds 0/1, redirects user stdout/stderr, optional TCP mode through `--omp-connect` (`_connect_address`).
- Both forms get the same sanitized environment, the same process group, the same sandbox wrapper (`crates/envd/src/eval/process.rs:586 command_for`) and `kill_on_drop`.
- Feature parity of `external_runner.py` against the embedded `omp.*`/`_omp` surface was not checked. `STATUS.md` and ADR 0036 do not mention this mode (a documentation gap, card PY-10).

Extension hosts and named workers have no external-interpreter mode: they always re-enter the same binary (`crates/envd/src/exthost/spawn.rs:584 command`).

## 4. Isolation for the extension host

`crates/envd/src/exthost/spawn.rs:543 spawn` has two tiers. `sandboxed`: native sandbox with read allow-list of only the executable, the Python site tree and the entry file, scoped write to the env socket and workspace, network disabled, degradation rejected (lines 553-577). `trusted`: plain child, no sandbox (line 578). The read allow-list is small on Linux (the executable, the site tree, the entry file). On macOS `crates/envd/src/exthost/spawn.rs:472 allow_loaded_runtime_images` walks the parent's loaded dylib images and additionally grants read on their directories and on package-manager prefixes such as `/opt/homebrew` (lines 502-518), so the binary is not fully self-contained there and the effective allow-list is wider. A standalone CPython would need its own prefix and stdlib read-allowed on top (not tested).

## 5. Other runtimes searched

| Runtime | Result | How searched |
|---|---|---|
| Standalone CPython | Eval only, opt-in (section 3) | read `crates/envd/src/eval/process.rs` |
| RustPython | Absent | `Cargo.lock` has no `rustpython` entry (parent check, `02-verified-current-state.md` section 7); documentation only otherwise |
| Native Rust executable extensions | Absent as an extension kind. Non-Python tools run as MCP stdio servers (`crates/envd/src/mcp/stdio.rs:1`) | ADR 0036 decision 2; ADR 0046 line 52 |
| Go executable adapters | Absent. Only a Go test fixture (`fixtures/sarif/sources/golangci/outcomes.go`) and a doc remark that a Maven or Go build adapter would be a new adapter (`crates/exec/README.md:38`) | `git ls-files '*.go' go.mod`; grep for `go adapter`, `go executable`, `go binary` in `crates`, `docs`, `extensions`, `AGENTS.md` |
| WebAssembly components | Absent | `Cargo.lock` has no `wasmtime`, `wasmer`, `extism` (parent check) |

## 6. What was and was not measured

Measured (see `evidence/measurements/`): vendored interpreter tree sizes (60), standalone CPython spawn cost (61), JSON-lines round trip to a persistent child (62). Not measured: embedded child startup, RSS of any child, binary size delta of `omp-py`, PyO3 callback cost, wheel coverage for 3.14t.

Back to the [index](../README.md).
