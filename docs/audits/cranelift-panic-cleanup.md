# Audit: panic cleanup in Cranelift-built dev and test code

| | |
|---|---|
| **Date** | 2026-09-23 (work ran 2026-09-22 23:29 UTC to 2026-09-23 UTC) |
| **Audited commit** | `77e279f64c277951374d1a8c52d5a7bed60224ad` (head of PR #32, branch `claude/omp2-audit-9twnv0`) |
| **Toolchain** | `nightly-2026-08-08`: rustc 1.99.0-nightly (`1a98b1e13` 2026-08-07), cargo 1.99.0-nightly (`c79e8f894` 2026-08-04), LLVM 23.1.0, `rustc-codegen-cranelift-preview` (cranelift-codegen 0.134.0), cargo-nextest 0.9.146 |
| **Host** | x86_64 Linux VM (kernel 6.18), Intel Xeon @ 2.10 GHz, **4 vCPUs** (1 thread per core), 15 GiB RAM, no swap, running as root |
| **Scope** | Every Cranelift-built workspace crate and its tests, as configured by `.cargo/config.toml` at this commit |

Every `file:line` below refers to the audited commit.

**Evidence labels.** **[M]** means measured or reproduced during this audit; the method is given alongside. **[I]** means inferred: from reading code, from upstream source, or by extrapolating from a measurement. The recommendation rests only on [M] items.

**Platform caveat.** Every local measurement ran on x86_64 Linux. The team's main development platform, aarch64 macOS, was not available. macOS evidence comes only from CI logs (§5) and from upstream Cranelift documentation (§6.3).

---

## 0. Summary

1. **[M] On this toolchain, Cranelift emits no landing pads.** With a minimal probe (§1):
   - Destructors in Cranelift frames are skipped during a panic.
   - A `catch_unwind` compiled in a Cranelift crate does not catch.
   - A panic on a `std::thread` spawned from Cranelift code aborts the process (SIGABRT).
   - A panicking tokio task aborts a multi-thread runtime. On a current-thread runtime the panic escapes `block_on` instead of becoming a `JoinError`.
   - A panicking `spawn_blocking` closure leaves its `JoinHandle` pending forever.
   - libtest's own catch and `#[should_panic]` do work.

   The same probe built all-LLVM behaves correctly in every case.
2. **[M] Inventory: 2,423 code sites in 33 Cranelift crates** use a mechanism that depends on unwinding. 644 were classified one by one: 322 are harmless, 105 are isolation hazards (37 of them high or medium severity) and 217 are cascading hazards (118 high or medium). The other 1,779 sites fall into two bulk classes: 1,379 temp-dir constructors and 400 tokio spawns (§2–§3).
3. **[M] Concrete defects reproduced in the real crates:**
   - `omp-shell-builtins`' panic boundary does not contain a builtin panic. In its production shape the shell command **hangs**.
   - A Rust panic in an `omp-py` `#[pyfunction]` unwinds straight through CPython instead of raising `PanicException`.
   - A `kill_on_drop` child outlives its failing test, and nextest does not report a LEAK.
   - A panicking tokio worker aborts an e2e test **even with the PR #32 hook installed**, and the test thread's process group is orphaned.
   - A `#[should_panic]` test in `omp-core` is vacuous: its assertions never run.
4. **[M] `omp-observability`: its LLVM override delivers the catch only partly.** Hooks invoked through the crate's non-generic methods are contained. But `TelemetryConfig::estimate_cost` is generic, so when a Cranelift crate calls it, its `catch_unwind` is instantiated in Cranelift and the panic escapes. A contained hook that panicked while holding a lock leaves that lock **held forever**. Without the override, even the non-generic path fails (§4).
5. **[M] The reported SIGSEGV is real.** Moving only `omp-e2e` to LLVM crashes `p6_resume_preserves_open_stream_prefix_without_inventing_completion` with SIGSEGV in 5 of 5 runs, inside `omp_core::str::Repr::tag`. With every workspace member on LLVM in the test profile (a homogeneous build), all 16 e2e tests pass (§6.2).
6. **[M] CI history shows both hazards on PR #32.** A tokio-worker panic became a SIGABRT four times (P7, Linux). Processes were orphaned after failing tests in 10 jobs (§5).
7. **[M] On this host, switching the backend barely changes build times**:

   | Build | Cranelift | LLVM | Difference |
   |---|---|---|---|
   | Cold `omp` build, mean of 3 | 405.8 s | 410.6 s | +1.2%, within noise |
   | Warm rebuild after an `omp-core` edit, mean of 3 | 233.4 s | 238.1 s | +2.0% |
   | Cold `omp-e2e` test build | 427.9 s | 427.6 s | none |

   The LLVM test build's target dir is **half the size**: 7.3 GB against 15 GB.

   One real cost remains: with dev on Cranelift and test on LLVM, member crates compile twice. The first `omp-e2e` test build after a warm dev build goes from 177 s to 259 s (§6.4).
8. **[M] Cranelift unwinding support (option c) is not usable with the pinned toolchain.** It is compiled out of the rustup component, and upstream documents it as unsupported on macOS (§6.3).

**Recommendation (§7):** make every test build homogeneously LLVM by adding `[profile.test] codegen-backend = "llvm"`, and keep the PR #32 hook as a second layer. This leaves `cargo build` and `cargo run` on Cranelift, so the cold `cargo run --bin omp` gate is untouched by construction. The follow-up PR is specified in §7.

---

## 1. Baseline: what Cranelift does with a panic [M]

**Probe.** I built a throwaway workspace (sources in Appendix C) with the repo's backend layout:
- workspace members on Cranelift;
- dependencies on LLVM through `[profile.dev.package."*"]`;
- one member (`llvmlib`) overridden to LLVM, mirroring `omp-observability`.

Every scenario runs as its own process with a 10 s timeout. The **control** is the same code built with `--config 'profile.dev.codegen-backend="llvm"'`.

| # | Scenario (where the code is compiled) | Cranelift build (as dev) | All-LLVM control |
|---|---|---|---|
| 1 | `catch_unwind` in a Cranelift crate, with a guard inside | **not caught**, the guard is not dropped, exit 101 (the panic reaches `main`) | caught, guard dropped |
| 2 | `std::thread::spawn` from a Cranelift crate, and the thread panics | **SIGABRT** (exit 134): `fatal runtime error: failed to initiate panic, error 5, aborting` | `join()` returns `Err` |
| 3 | Non-generic `catch_unwind` in an LLVM crate calling a Cranelift `dyn Fn` that panics | caught; the Cranelift frame's guard is **not dropped** | caught, guard dropped |
| 4 | **Generic** `catch_generic<F>` defined in the LLVM crate, called with a Cranelift closure | **not caught**, exit 101 (the instantiation is compiled in the Cranelift caller) | caught |
| 5 | The same generic with `F = fn()`, already instantiated inside the LLVM crate | caught (**[I]** reason: the caller reuses the upstream instantiation through share-generics) | caught |
| 6 | A guard in an LLVM frame while the panic passes through | dropped | dropped |
| 7 | `thread::spawn` inside a non-generic LLVM function | `join()` returns `Err` | `join()` returns `Err` |
| 8 | `tokio::spawn` on a multi-thread runtime, and the task panics | **SIGABRT**: `worker thread panicking; aborting process` | `JoinError::is_panic()` |
| 9 | `tokio::task::spawn_blocking`, and the closure panics | **hangs**: the `JoinHandle` never resolves (killed by the 10 s timeout, exit 124) | `JoinError::is_panic()` |
| 10 | `tokio::spawn` on a current-thread runtime | the panic **escapes `block_on`**, exit 101; no `JoinError` is produced | `JoinError::is_panic()` |
| 11 | `panic::set_hook` | the hook runs | the hook runs |
| 12 | libtest `#[test]` that panics while holding a guard that writes a marker file | reported `FAILED` normally; **the marker file is never written** (Drop skipped) | `FAILED`, marker written |
| 13 | `#[should_panic]` | passes | passes |

**What this means.** Unwinding itself works: Cranelift writes unwind tables, so a panic travels up the stack and is caught by any catch compiled in LLVM code, including libtest's own catch. What is missing is every landing pad inside Cranelift frames:
- no `Drop` runs in those frames;
- no `catch_unwind` compiled there catches.

Which backend compiles a given catch depends on **where the generic is instantiated**, not where it is written. This matters for rows 4 and 5, and for `std::thread::spawn` and tokio's task harness, which are generic over the closure or future. Row 9's hang is explained by that too **[I]**:
- The blocking pool's worker loop lives in tokio, which is compiled with LLVM. It survives the panic.
- The task harness that should have recorded the panic was instantiated in the Cranelift crate, and it never ran its landing pad.
- So the thread keeps serving the pool while the task's completion is never published.

**Why (upstream source, [I]).** In the pinned compiler's `compiler/rustc_codegen_cranelift/src/abi/mod.rs` (commit `1a98b1e1`), `codegen_call_with_unwind_action` does this:

```rust
if cfg!(not(feature = "unwinding")) {
    unwind = UnwindAction::Unreachable;
}
```

`Cargo.toml` of the same commit declares that feature like this:

```toml
unwinding = [] # Not yet included in unstable-features for performance reasons
```

The shipped backend binary (`librustc_codegen_cranelift-1.99.0-nightly.so`) contains the exception-table code, but the feature is compiled out, as the probe confirms.

---

## 2. Inventory

### 2.1 Method (re-derivable)

**Scope.** The scope is everything under `crates/*`, excluding:
- packages built with LLVM: `crates/ar` (`omp-ar`), `crates/con` (`omp-con`) and `crates/observability` (`omp-observability`);
- the proc-macro crate `crates/macros`, which is compiled for the host through `[profile.dev.build-override]`;
- every `build.rs`.

Test code in `src/**` and `tests/**` is included.

`.cargo/config.toml` also sets an LLVM override on `omp-con` (linkme slices), which the task background did not mention. It is excluded for the same reason.

**Search script.** This is the exact script. It needs ripgrep; it was run from the repo root.

```bash
RG=(rg --no-heading -n --type rust -g '!crates/ar/**' -g '!crates/con/**' \
    -g '!crates/observability/**' -g '!crates/macros/**' -g '!**/build.rs')
declare -A PAT=(
 [A_catch_unwind]='catch_unwind'
 [B_panic_hook]='panic::(set_hook|take_hook|update_hook)'
 [C_should_panic]='#\[should_panic'
 [D_join_panic]='JoinError|\.is_panic\(\)|into_panic|try_into_panic|resume_unwind'
 [E_std_thread]='thread::(spawn|scope)\b|thread::Builder::new'
 [F_spawn_blocking]='spawn_blocking'
 [G_tokio_spawn]='tokio::spawn\(|tokio::task::spawn\(|JoinSet|task::spawn_local\(|\.spawn_local\('
 [H_multi_thread_rt]='flavor\s*=\s*"multi_thread"|new_multi_thread'
 [I_drop_impl]='impl(<[^>]*>)?\s+(\w+::)*Drop\s+for\b'
 [J_kill_on_drop]='kill_on_drop'
 [K_tempfs]='tempfile::(tempdir|tempdir_in|tempfile|NamedTempFile|Builder)|TempDir::(new|with_prefix|new_in)|NamedTempFile::|tempdir\(\)|tempdir_in\('
 [L_terminal]='tcsetattr|enable_raw_mode|disable_raw_mode|EnterAlternateScreen|LeaveAlternateScreen|Termios'
 [M_socket_bind]='UnixListener::bind|TcpListener::bind|UnixDatagram::bind|UdpSocket::bind'
 [N_scopeguard]='scopeguard|defer!|ScopeGuard'
)
for k in $(printf '%s\n' "${!PAT[@]}" | sort); do
  "${RG[@]}" -e "${PAT[$k]}" crates | sed "s|^|$k\t|"
done > inventory.tsv
```

**Filtering.** The raw output has 2,474 lines. A line was dropped if its code, after trimming, starts with `//` (a comment) or with `use` or `pub use` / `pub(…) use` (an import). That leaves **2,423 sites**.

A site is one matching source line. A line matching two patterns counts once under each: `crates/shell/src/env.rs:84` is both an `impl Drop` and a `scopeguard` site. Category A contains one import continuation line, `crates/shell-builtins/src/host.rs:31`, which a line-based filter cannot drop.

**Coverage limits [I].** Patterns cannot see guards built by macros or helper constructors that match none of the patterns. Some RAII types are also used through constructors, and only their `impl Drop` is counted, not every use site. The classification agents followed call sites where they mattered; the high-severity rows name the owning constructors.

### 2.2 Per-crate counts (raw sites after filtering) [M]

Columns:
- **A** `catch_unwind`
- **B** panic hook
- **C** `should_panic`
- **D** `JoinError` / `is_panic` / `resume_unwind`
- **E** std thread
- **F** `spawn_blocking`
- **G** tokio spawn
- **H** multi-thread runtime
- **I** `impl Drop`
- **J** `kill_on_drop`
- **K** tempfile constructor
- **L** terminal / termios
- **M** socket bind
- **N** scopeguard / defer

| Crate | A | B | C | D | E | F | G | H | I | J | K | L | M | N | total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `agent` | 0 | 0 | 0 | 1 | 0 | 0 | 33 | 0 | 4 | 0 | 102 | 0 | 0 | 0 | 140 |
| `ai` | 2 | 0 | 0 | 2 | 3 | 3 | 25 | 0 | 27 | 1 | 50 | 0 | 13 | 0 | 126 |
| `app` | 0 | 1 | 0 | 3 | 11 | 9 | 46 | 0 | 12 | 1 | 163 | 2 | 5 | 0 | 253 |
| `audio` | 0 | 0 | 0 | 0 | 7 | 0 | 0 | 0 | 20 | 0 | 0 | 0 | 0 | 0 | 27 |
| `cache` | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 1 | 0 | 15 | 0 | 0 | 0 | 17 |
| `chat` | 0 | 0 | 0 | 0 | 4 | 1 | 2 | 2 | 5 | 1 | 73 | 0 | 0 | 0 | 88 |
| `core` | 1 | 0 | 21 | 1 | 3 | 0 | 0 | 0 | 10 | 0 | 0 | 0 | 0 | 0 | 36 |
| `desktop` | 1 | 0 | 0 | 0 | 3 | 2 | 0 | 0 | 7 | 0 | 1 | 0 | 1 | 0 | 15 |
| `driver` | 0 | 0 | 0 | 0 | 1 | 0 | 24 | 0 | 6 | 2 | 79 | 0 | 0 | 0 | 112 |
| `e2e` | 0 | 3 | 0 | 0 | 3 | 0 | 5 | 4 | 8 | 1 | 17 | 4 | 0 | 0 | 45 |
| `edit` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 14 | 0 | 0 | 0 | 14 |
| `env` | 0 | 0 | 0 | 0 | 17 | 0 | 13 | 0 | 8 | 0 | 1 | 0 | 2 | 0 | 41 |
| `envd` | 0 | 0 | 0 | 17 | 21 | 52 | 214 | 0 | 75 | 11 | 314 | 0 | 30 | 0 | 734 |
| `ext` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 6 | 0 | 0 | 0 | 7 |
| `gui` | 0 | 0 | 0 | 0 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 3 |
| `journal` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 26 | 0 | 0 | 0 | 27 |
| `memory` | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 2 |
| `oauth` | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 4 | 0 | 5 |
| `py` | 0 | 0 | 0 | 1 | 1 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 3 |
| `rpc` | 0 | 0 | 0 | 0 | 0 | 0 | 9 | 0 | 0 | 1 | 3 | 0 | 1 | 0 | 14 |
| `sandbox` | 0 | 0 | 0 | 0 | 6 | 1 | 5 | 0 | 16 | 1 | 78 | 0 | 10 | 0 | 117 |
| `sdk` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 1 |
| `secrets` | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 2 |
| `serve` | 0 | 0 | 0 | 1 | 0 | 4 | 3 | 0 | 1 | 0 | 0 | 0 | 0 | 0 | 9 |
| `session` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 45 | 0 | 0 | 0 | 45 |
| `shell` | 0 | 0 | 0 | 1 | 1 | 2 | 8 | 0 | 7 | 2 | 11 | 3 | 0 | 5 | 40 |
| `shell-builtins` | 2 | 0 | 1 | 1 | 9 | 1 | 0 | 0 | 10 | 0 | 311 | 0 | 0 | 0 | 335 |
| `tool` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 0 | 0 | 0 | 0 | 0 | 3 |
| `tools` | 0 | 0 | 0 | 0 | 4 | 6 | 6 | 0 | 12 | 0 | 16 | 0 | 0 | 0 | 44 |
| `tui` | 0 | 2 | 2 | 0 | 14 | 1 | 4 | 1 | 7 | 0 | 2 | 16 | 2 | 0 | 51 |
| `vcs` | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 1 | 49 | 0 | 0 | 0 | 52 |
| `walker` | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 7 | 0 | 0 | 0 | 0 | 0 | 7 |
| `webview` | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 | 4 | 2 | 1 | 0 | 0 | 0 | 8 |
| **total** | **6** | **6** | **24** | **28** | **115** | **82** | **400** | **8** | **251** | **26** | **1379** | **25** | **68** | **5** | **2423** |

---

## 3. Classification

### 3.1 Classes and how rows were assigned

- **Harmless**: what is skipped only matters inside the test process, and that process exits anyway. Under nextest every test runs in its own process. This covers memory, file descriptors, in-process locks, threads and tasks, ports this process bound, and fire-and-forget hooks.
- **Isolation hazard**: the leak outlives the test process. Examples: orphaned child processes, process groups or daemons; files, locks or sockets in fixed or shared locations; the termios, raw-mode or alternate-screen state of a real TTY. Leftover *uniquely named* temp dirs count here too, at low severity, because they only cost disk.
- **Cascading hazard**: an abort, a hang or a skipped cleanup turns one failure into many, or hides the original failure. Examples:
  - a panic on a std thread or a tokio multi-thread worker, which aborts the whole process;
  - a `spawn_blocking` hang;
  - a `catch_unwind` or `JoinError` path that can no longer be taken;
  - a lock whose guard is never released while the process keeps running;
  - a test whose assertions silently never run.

**Assignment.** The 644 sites in categories A–F, H–J and L–N were classified one row at a time. Four read-only agents did this under a common written rubric, with the facts from §1 as ground truth. Each row records its scope (`test`, or `prod` meaning it is built into the dev `omp` binary), a class, a severity and what the mechanism protects. I spot-checked every high-severity row and a random sample of the others against the code. The full table is Appendix A.

The two bulk categories are classified by rule, and the rows that break the rule are listed:

- **K, tempfile constructors (1,379 sites): isolation, low.**
  - **[M]** A `TempDir` owned by a failing test is not removed. The envd probe left `/tmp/.tmpAXaKVH` and `/tmp/.tmpX0Ehvh` behind under Cranelift; the LLVM control removed its own.
  - Exceptions where the path is fixed or lands beside user files:
    - `crates/envd/src/grep.rs:1515`: a test helper with its own `TempDir` type that creates `/tmp/omp-grep-<pid>-<n>`, used at `:1641 :1672 :1688 :1705 :1806`. A later process that reuses the PID reuses the directory with its stale contents.
    - `crates/shell-builtins/src/sed.rs:7554` (`sed -i`) and `crates/shell-builtins/src/jq.rs:1141` (`jq -i`) create in-place temp files next to the user's own file.
    - `crates/vcs/src/git/mutate.rs` `detach_git_dir_does_not_mutate_when_index_snapshot_fails` uses a fixed sibling name. PR #32 already lists it as a known issue.
- **G, tokio spawns (400 sites): cascading, low by default.**
  - On the dev `omp` binary's multi-thread runtime (`#[tokio::main]`, `crates/app/src/main.rs:60`), a panicking task aborts the process. **[M]** for the mechanism (probe row 8, and CI in §5).
  - In `#[tokio::test]` bodies, which default to a current-thread runtime, the panic still fails the test with its original message.
  - Where code relies on a task panic turning into a `JoinError`, the handler is a D row in Appendix A, raised to medium. Examples are `crates/agent/src/dispatch.rs:1032` and `crates/app/src/daemon.rs:160`. The supervisors that await those tasks include `crates/envd/src/server.rs:3768`, `crates/envd/src/docserver/daemon.rs:550` and `crates/shell/src/results.rs:280`. On this toolchain those branches are dead code in dev builds **[I, via mechanism M]**.

### 3.2 Per-crate classification [M for the counts]

Columns: rows per class. Numbers in parentheses count rows of high or medium severity. K and G are the bulk classes above.

| Crate | harmless | isolation (hi/med) | cascading (hi/med) | K bulk | G bulk | total |
|---|---:|---:|---:|---:|---:|---:|
| `agent` | 4 | 0 (0) | 1 (1) | 102 | 33 | 140 |
| `ai` | 40 | 1 (0) | 10 (8) | 50 | 25 | 126 |
| `app` | 18 | 5 (1) | 21 (8) | 163 | 46 | 253 |
| `audio` | 20 | 0 (0) | 7 (5) | 0 | 0 | 27 |
| `cache` | 1 | 0 (0) | 1 (0) | 15 | 0 | 17 |
| `chat` | 6 | 2 (0) | 5 (0) | 73 | 2 | 88 |
| `core` | 30 | 0 (0) | 6 (0) | 0 | 0 | 36 |
| `desktop` | 5 | 3 (1) | 6 (5) | 1 | 0 | 15 |
| `driver` | 5 | 3 (2) | 1 (0) | 79 | 24 | 112 |
| `e2e` | 10 | 8 (6) | 5 (5) | 17 | 5 | 45 |
| `edit` | 0 | 0 (0) | 0 (0) | 14 | 0 | 14 |
| `env` | 8 | 2 (0) | 17 (14) | 1 | 13 | 41 |
| `envd` | 84 | 34 (10) | 88 (46) | 314 | 214 | 734 |
| `ext` | 0 | 1 (0) | 0 (0) | 6 | 0 | 7 |
| `gui` | 0 | 0 (0) | 3 (0) | 0 | 0 | 3 |
| `journal` | 0 | 1 (0) | 0 (0) | 26 | 0 | 27 |
| `memory` | 0 | 1 (0) | 1 (1) | 0 | 0 | 2 |
| `oauth` | 4 | 0 (0) | 0 (0) | 0 | 1 | 5 |
| `py` | 0 | 0 (0) | 3 (3) | 0 | 0 | 3 |
| `rpc` | 0 | 2 (1) | 0 (0) | 3 | 9 | 14 |
| `sandbox` | 9 | 18 (7) | 7 (1) | 78 | 5 | 117 |
| `sdk` | 0 | 0 (0) | 0 (0) | 1 | 0 | 1 |
| `secrets` | 0 | 0 (0) | 1 (0) | 1 | 0 | 2 |
| `serve` | 3 | 0 (0) | 3 (0) | 0 | 3 | 9 |
| `session` | 0 | 0 (0) | 0 (0) | 45 | 0 | 45 |
| `shell` | 14 | 5 (2) | 2 (2) | 11 | 8 | 40 |
| `shell-builtins` | 13 | 0 (0) | 11 (8) | 311 | 0 | 335 |
| `tool` | 3 | 0 (0) | 0 (0) | 0 | 0 | 3 |
| `tools` | 11 | 1 (0) | 10 (6) | 16 | 6 | 44 |
| `tui` | 26 | 12 (5) | 7 (4) | 2 | 4 | 51 |
| `vcs` | 0 | 1 (0) | 0 (0) | 49 | 2 | 52 |
| `walker` | 4 | 3 (0) | 0 (0) | 0 | 0 | 7 |
| `webview` | 4 | 2 (2) | 1 (1) | 1 | 0 | 8 |
| **total** | **322** | **105 (37)** | **217 (118)** | **1379** | **400** | **2423** |

Split by scope: cascading is 162 prod and 55 test rows; isolation is 80 prod and 25 test; harmless is 236 prod and 86 test. Most **prod** hazards affect only people running a dev (Cranelift) `omp`. Release builds use LLVM (`[profile.release]` sets no backend) **[I]**.

### 3.3 The hazards that matter

Each item gives its evidence label and reproduction method. Appendix D has the reproduction commands.

**Cascading, reproduced in the real crates**

1. **The `omp-shell-builtins` panic boundary is gone, and shell commands hang. [M]**
   - Sites: `run_caught` at `crates/shell-builtins/src/host.rs:1049–1066`, its `spawn_blocking` at `:991`, and the dead `Err` arm at `:1029`.
   - Mechanism: `run_caught<U: Utility>` is generic, so its `catch_unwind` is instantiated in Cranelift.
   - Probe: a temporary test calling `run_caught` with a panicking `Utility`. Under Cranelift the probe's own `panic!` escaped `run_caught` instead of returning exit 1. Called the way production calls it, inside `spawn_blocking`, the awaiting `JoinHandle` was still **unresolved after 10 s**.
   - LLVM control: `run_caught returned 1; stderr="boom: internal error\n"`, and the `spawn_blocking` resolved with `Ok(1)`.
   - Related **[I]**: the `PANIC_SCOPE_DEPTH` guard at `:1051` is not dropped, so a pooled thread stays marked "in panic scope", and later genuine panics on it are kept out of crash reports (`panic_scope_active`, `:779`). The same hang shape appears in `crates/shell/src/commands.rs:523` (pipelines) and in the untrusted-input decoders at `crates/tools/src/read.rs:1133,1540,1561` and `crates/tools/src/read/web.rs:152`.
   - No existing test exercises builtin panic containment, so CI cannot see this.
2. **An `omp-py` `#[pyfunction]` panic tears through CPython. [M]**
   - Probe: a temporary test registers a panicking `#[pyfunction]` and calls it from Python inside `try: … except BaseException`.
   - LLVM: Python observed `PanicException`.
   - Cranelift: Python's `except` never ran, and the Rust panic surfaced in libtest, having unwound through the interpreter's C frames.
   - Why **[I]**: pyo3's panic trampoline is generic, so it is instantiated in the Cranelift crate. Unwinding through foreign frames is undefined behavior, and the interpreter's state after that is unspecified.
   - Scope **[I]**: this covers every binding in `crates/py/src/bindings.rs` (57 pyo3 attributes) and `crates/tools/src/eval/kernel.rs`.
3. **A tokio worker panic in an e2e proof aborts the test even with the PR #32 hook, and orphans the test thread's groups. [M]**
   - Probe: a temporary multi-thread `#[tokio::test]` leases an `OwnedProcess` (`/bin/sleep 3232`) on the libtest thread, then awaits a task that panics.
   - Cranelift: `SIGABRT`, `worker thread panicking; aborting process`, and `sleep 3232` is left running with ppid 1. The hook ran on the worker thread, which owned no groups.
   - LLVM (both `omp-e2e` alone and the whole test profile): the test passes and nothing is left behind.
   - This is the limit the module doc states, and it is exactly the P7 CI failure shape in §5. The proofs that run gateways on worker threads are `crates/e2e/tests/p1_doc_race.rs:22`, `crates/e2e/tests/p6_crash_resume.rs:480` and `crates/e2e/tests/p7_tui.rs:925,1118`.
4. **A vacuous `should_panic` test in `omp-core`. [M]**
   - Test: `test_extend_publishes_reported_prefix_before_over_yield_panic` (`crates/core/src/append_vec.rs:1336–1349`) asserts the published prefix after a `catch_unwind`, then resumes the panic.
   - Mutation: changing `assert_eq!(vec.len(), 2)` to `999`.
   - Cranelift: the test still **PASSES**, because the catch never engages and `should_panic` is satisfied by the original panic.
   - LLVM: the test **FAILS** with `left: 2, right: 999`.
   - Its sibling at `:1352–1361` carries a comment saying it was written around this limitation.
5. **Std threads in prod paths abort the dev `omp` [I, via mechanism M].** Examples: the desktop worker, whose `catch_unwind` at `crates/desktop/src/lib.rs:741` sits inside a Cranelift-spawned thread (`:730`); the browser actor at `crates/envd/src/browser_daemon.rs:237`; the Python eval worker at `crates/tools/src/eval/kernel.rs:1138`; the sandbox proxy parsers at `crates/envd/src/sandbox_proxy.rs:228,240,648`; the webview driver at `crates/webview/src/remote/mod.rs:198`; and the audio, clipboard, spelling and sort helper threads (Appendix A).
6. **Test fixture threads with assertions [I, via mechanism M].** A failed assertion becomes a SIGABRT that loses the assertion message, instead of a clean FAIL. Examples: `crates/envd/src/sandbox_proxy.rs:1194,1259,1334,1369,1419`, the 13 server threads in `crates/env/tests/client.rs`, and `crates/ai/src/auth/store.rs:2014`.

**Isolation, reproduced**

7. **A `kill_on_drop` child owned by a failing test survives, and nextest cannot see it. [M]**
   - Probe: a temporary test in `omp-envd` spawns `/bin/sleep 3131` with `kill_on_drop(true)` and null stdio, then panics.
   - Cranelift: FAIL, and the `sleep 3131` process was still running afterwards, reported as FAIL **without LEAK**. nextest detects leaks only through inherited stdout or stderr.
   - LLVM: the child was killed.
   - The 26 `kill_on_drop` sites and the child-owning `Drop` impls are in Appendix A. The highest-severity ones:
     - sandbox children in their own process group: `crates/sandbox/src/runner.rs:892,1069` and `crates/sandbox/src/runtime/gvisor.rs:741`;
     - Docker and gVisor container teardown: `crates/sandbox/src/runtime/docker.rs:183` and `crates/sandbox/src/runtime/gvisor.rs:124`;
     - MCP server process groups: `crates/envd/src/mcp/stdio.rs:420,468`;
     - exec'd commands: `crates/envd/src/exec.rs:250`;
     - DAP adapters: `crates/envd/src/docserver/dap_protocol.rs:471`;
     - browsers: `crates/webview/src/remote/chromium.rs:407` and `firefox.rs:219`.
8. **Leaked temp dirs. [M]** See §3.1, K.
9. **Only by reading [I]:**
   - Stale lock files that block later runs: `crates/app/src/update_cmd.rs:201` (`update.lock`) and `crates/envd/src/mcp/config_store.rs:421` (`.mcp-config.lock`, which has no stale-lock recovery).
   - Termios or raw mode left on a real TTY: `crates/tui/src/graphics.rs:756–797`, where the startup probe restores by hand before the panic hook is active, and `crates/shell/src/builtins/terminal.rs:41`.
   - `Terminal::drop` (`crates/tui/src/terminal.rs:2181`) is skipped. The chained emergency-restore panic hook at `:2380` is now the only thing that restores the terminal, and it must be kept.

**Negative result, recorded so no one repeats it.** A panicking test that owned a process started through `omp_envd::exec::ExecHost::start_process` did **not** leave that process running on this host **[M]**. The process re-execs the test binary as a detached, process-group-owning child (`crates/envd/src/exec.rs:1269–1294`, `:3363`). I did not determine what kills it (**[I]** unknown), so this audit claims no leak for `ExecHost`-started processes.

---

## 4. `omp-observability` [M]

**The catch sites.** The fail-open catches are:
- `invoke` and `invoke2`, generic over the hook type: `crates/observability/src/config.rs:669,676`;
- a direct catch in `telemetry_warning`: `config.rs:640`.

All hooks are `Arc<dyn Fn…>`. So the catch is instantiated inside `omp-observability` whenever the public entry point is non-generic.

**Who calls them.** At this commit, **no crate outside `omp-observability` builds a `TelemetryConfig`** (`rg TelemetryConfig crates -g '!crates/observability/**'` finds nothing). The only tests of the fail-open hooks are the crate's own unit tests, and they are compiled with LLVM, so they cannot reveal a Cranelift-caller problem.

**Probe.** A temporary integration test in `omp-session`, a Cranelift crate that depends on `omp-observability`, with hook closures defined in that Cranelift crate:

| Case | Current config (override on) | Override removed (`--config 'profile.dev.package.omp-observability.codegen-backend="cranelift"'`) | All-LLVM control |
|---|---|---|---|
| `normalized_provider` with a panicking `normalize_provider` hook (non-generic entry, `dyn` hook) | **contained**: fallback value, one `NormalizeProviderFailed` warning | **not contained**: the panic escapes (`zz_observability_probe.rs:26`) | contained |
| `estimate_cost(&ctx, \|_\| …)` with a panicking `cost_estimator` (**generic** entry, `impl FnOnce`, `config.rs:457`) | **not contained**: the panic escapes (`zz_observability_probe.rs:39`) | not contained | contained: `Some(catalog)` plus a `CostEstimatorFailed` warning |
| A warning hook that locks a `std::sync::Mutex`, then panics (contained through `telemetry_warning`) | contained, but the **mutex is STILL LOCKED** afterwards (`try_lock` returned `WouldBlock`) | not contained | contained; the mutex is **poisoned**, meaning its guard was dropped during the unwind |

**Answers.**
- **Does the override deliver the `catch_unwind` the hooks need?** Only partly. It works for every non-generic `TelemetryConfig` method **[M]**. It does not work for `estimate_cost`, whose `impl FnOnce` parameter makes the whole method, and the `invoke` call inside it, get instantiated in the caller **[M]**. `in_active_span<T>` (`crates/observability/src/span.rs:377`) is generic too but has no catch. The fix, if the override stays, is to make `estimate_cost` take `&dyn FnOnce…` or to move the catch behind a non-generic inner function. The recommendation in §7 makes that unnecessary.
- **Even where the catch works, cleanup inside the hook does not happen [M].** Any guard, lock or RAII value held by a Cranelift-compiled hook closure is leaked. A process that keeps running after a contained panic, which is the whole point of failing open, can then deadlock on that lock.
- **Other crates with the same need but no override [M/I]:**
  - `omp-shell-builtins` (`run_caught`): [M], §3.3 item 1.
  - `omp-py` (the pyo3 trampolines): [M], §3.3 item 2.
  - `omp-desktop` (the worker's `catch_unwind`, `crates/desktop/src/lib.rs:741`): [I].
  - `omp-ai`: the `catch_unwind` in an `extern "C"` Apple Foundation Models callback at `crates/ai/src/local/applefm/platform.rs:1248`. It is macOS-only, and there a panic crossing into Swift aborts [I].
  - Every crate that relies on tokio's `JoinError` or on `std::thread` join `Err` for panic isolation (category D in Appendix A): [M] for the mechanism.

---

## 5. Secondary flakes in CI history [M]

**Coverage.** The branch `claude/omp2-audit-9twnv0` (PR #32) has 58 CI runs, #114–#172: 38 failed, 19 cancelled and 1 succeeded (#172, the audited commit). Branch `omp2` has no CI runs. Full log archives were downloaded for all 57 failed or cancelled runs and searched for the following signatures:
- `LEAK`, `SIGABRT`, `SIGSEGV`, `aborting`, `failed to initiate panic`, `worker thread panicking`;
- `TIMEOUT`, `Address already in use`, `File exists`, `orphan`, `hang`.

The only gap is attempt 1 of run #158, whose archive holds only attempt 2.

| Observation | Runs | Verbatim evidence | Assessment |
|---|---|---|---|
| **A tokio-worker panic aborts P7 on Linux** | #130–#133 ([35569215373](https://github.com/blyzer/oh-my-pi/actions/runs/35569215373), [35570589295](https://github.com/blyzer/oh-my-pi/actions/runs/35570589295), [35571703527](https://github.com/blyzer/oh-my-pi/actions/runs/35571703527), [35574056921](https://github.com/blyzer/oh-my-pi/actions/runs/35574056921)) | `thread 'tokio-rt-worker' (61831) panicked at crates/e2e/tests/p7_tui.rs:95:14:` … `worker thread panicking; aborting process` … `(test aborted with signal 6: SIGABRT)` followed by `Terminate orphan process: pid (61835) (omp_e2e_host)` (×3) | The trigger was a real test/production mismatch, fixed by `ca65e44`. **The abort and the three orphans are consequences of skipped cleanup.** Only the worker's panic message survived. |
| **P7 FAIL leaves processes behind** (no abort; the test thread panicked) | #119, #124, #125, #126, #128 (Linux) | `Terminate orphan process: pid (…) (omp_e2e_host)`, 2–3 per job | Consequence: each FAIL left `omp chat` hosts running until the runner killed them. None appear after P7 goes green (#134–#171). |
| **P6 FAIL + LEAK on macOS** | #167 ([35714569342](https://github.com/blyzer/oh-my-pi/actions/runs/35714569342)) | `FAIL + LEAK [  38.699s] (10/11) omp-e2e::p6_crash_resume p6_killed_real_streaming_omp_resumes_durable_prefix_through_cli`, then two `Terminate orphan process … (omp_e2e_host)` | The root cause (an undrained PTY) was fixed by `286a951`. The LEAK and the orphans are the motivation for the PR #32 hook (`3371aff`), and they show that the hazard exists on macOS as well. |
| `LEAK` on a *passing* test | #166 | `LEAK [   0.392s] (4618/9031) omp-envd server::tests::host_keyed_connections_cannot_invoke_eval` | Unclear; there was no panic, so it is not attributable to Cranelift. |

**Ruled out.** None of these appear in any log: `failed to initiate panic`, `fatal runtime error`, SIGSEGV, `EADDRINUSE`, `File exists`. The two TIMEOUTs, in #147 and #149 (`exec::tests::fast_output_is_host_bounded_and_complete_in_the_spill_artifact`), show no panic and were fixed by `b622356`, so they are real bugs and not a post-panic `spawn_blocking` hang.

**Intermittent failures I could not attribute:**
- `pause_holds_a_running_turn…` (#147, #149, #153, #163)
- `sandbox_proxy headers_are_bounded…` with `Connection reset by peer` (#150, #166)
- `glob_multi_root…` (#143)
- the eval-cancel timeout (#164)

None of their logs contain an abort or orphan signature.


### 5.1 Local observation: temp dirs left behind by the suite [M]

After this audit's runs, `$TMPDIR` held 234 `/tmp/.tmp*` directories. Grouped by modification time and contents:

- **214 came from the full workspace run under the LLVM test profile** (9,041 tests). They are session journals (`commands.oms`, `keys.oms`, `fixture.oms`, `project.oms`, …) from tests that call `TempDir::keep()` on purpose, for example:
  - `crates/chat/tests/pi_commands.rs:139,226`
  - `crates/chat/tests/keys.rs:67`
  - `crates/chat/tests/host.rs:22`
  - `crates/chat/src/project.rs:1294`
  - `crates/driver/tests/workpool_scheduler.rs:180`

  These leak on **every** run, whatever the backend, and grow `$TMPDIR` by about 200 directories per run. This is not a Cranelift issue, but it is an isolation hazard of the same kind.
- **10 came from rerunning the 10 environment-failing tests under Cranelift.** Each panicking test left its `TempDir` behind. This is the K class of §3.1.
- **8 came from this audit's probe and e2e runs.**

---

## 6. Remedies

### 6.1 (a) Panic hook plus registry, extended

**Coverage.**
- **[M]** At HEAD, the PR #32 hook works for what it targets. `owned_groups::tests::panic_cleanup_is_scoped_…` passes, and with the hook disabled by mutation (`HOOK.call_once(install_hook)` removed) the test fails at `owned_groups.rs:210` and leaves `/bin/sleep 300` orphaned with ppid 1.
- **[M]** It does not cover a panic on a thread other than the owner's: §3.3 item 3.
- **[M]** Its reach is structural. A hook can send signals and write to files, but it cannot:
  - make a `catch_unwind` catch (§3.3 items 1 and 2, §4);
  - make `JoinError` or join `Err` exist, which is the dead-branch class D;
  - un-hang a `spawn_blocking` (probe row 9);
  - release a lock held by a frame that was skipped (§4);
  - make a vacuous test meaningful (§3.3 item 4).
- Extending it would mean a registry per resource kind: processes, container IDs, lock files, termios snapshots, temp dirs. Each would need its own identity rules, like the careful PID-reuse argument in `owned_groups.rs:15–21`. That covers at most the **isolation** class (105 classified rows plus 1,379 temp-dir sites), and **none of the 217 cascading rows**.

**Other properties.** It keeps the backend configuration uniform, so there are no boundary crashes. It costs no build time.

**Maintenance [I]:** high. Every new RAII resource needs a registry entry, and missing one fails silently. Two examples: nextest shows no LEAK when stdio is null (§3.3 item 7), and P7's `PtyChild` holds a plain `std::process::Child` with no lease (`crates/e2e/tests/p7_tui.rs:653`).

### 6.2 (b) A single backend: LLVM for some crates, or for all test builds

**Per-crate LLVM (for example `omp-e2e` alone): rejected on evidence [M].**
- With `--config 'profile.dev.package.omp-e2e.codegen-backend="llvm"'`, I checked with `cargo test -v` that only `omp_e2e`, `omp_e2e_host` and the e2e test crates were built with `-Z codegen-backend=llvm`; the other 967 units were unchanged Cranelift artifacts.
- `p6_crash_resume::p6_resume_preserves_open_stream_prefix_without_inventing_completion` then crashed with **SIGSEGV in 5 of 5 runs**. It passes under the default configuration.
- gdb backtrace:

  ```
  #0 <omp_core::str::Repr>::tag ()        => movzbq 0x17(%rdi),%rax
  #1 <omp_core::str::Repr>::view ()  … #4 <omp_core::str::Str as Serialize>::serialize
  #8 <omp_journal::data::MsgAssistantStart as Serialize>::serialize
  #12 <omp_session::session::Session>::commit::<MsgAssistantStart>
  #13 <omp_session::session::Session>::assistant_start::<&str, &str, &str>
  #14 p6_crash_resume::p6_resume_preserves_open_stream_prefix_without_inventing_completion
  ```

- **[I]** `Repr` is a 24-byte `#[repr(C)] union` (`crates/core/src/str.rs:1104`). `assistant_start` is generic, so it is instantiated in the LLVM crate, and the `Str` it builds is read by Cranelift-compiled code through an invalid pointer. That is consistent with the two backends disagreeing on how some aggregate is passed or returned by value. I did not isolate which call. This is the "deterministic SIGSEGV at an LLVM↔Cranelift boundary" reported in PR #32, now reproduced on x86_64 Linux.
- **[I]** The existing per-crate overrides (`omp-ar`, `omp-con`, `omp-observability`) create the same kind of boundary today. CI is green, so no instance is triggered at this commit, but nothing guards against one.

**Every test build on LLVM (`[profile.test] codegen-backend = "llvm"`): measured.**
- Scope, checked with `cargo test -v` on the probe workspace: `cargo test` and `cargo nextest` build **every** unit in the `test` profile, library dependencies of test binaries included. So this one key makes test builds homogeneously LLVM. `cargo build` and `cargo run` still use `dev`, so the dev `omp` stays on Cranelift **[M]**.
- **Correctness [M]:**
  - Every in-repo probe from §3–§4 passes: `run_caught` is contained, `spawn_blocking` resolves, both observability entry points contain the panic, the lock is poisoned rather than left held, and the `kill_on_drop` child is killed.
  - The pyo3 panic becomes `PanicException`.
  - The vacuous test starts testing.
  - e2e: `--lib` plus P1–P7 plus the worker-panic probe, **16 of 16 passed**, with nothing left running.
  - The whole workspace suite (`cargo nextest run --workspace --exclude omp-e2e`) gave **9,031 passed and 10 failed**. All 10 also fail when rerun under the current Cranelift configuration (`-p omp-envd -p omp-vcs`, same 10 tests), so they do not come from the backend. The likely environmental causes are **[I]**: the host runs as root, which bypasses permission fixtures; the host git rejects `--ref-format=reftable` (seen in the log); and IPv6 loopback or the browser relay in this container.
  - Not run: doctests (`cargo test --doc`) under this profile.
- **Boundary risk [M/I]:** none inside test builds, because the build is homogeneous. The e2e suite that crashed under the per-crate override passes.
- **The PR #32 hook stays valid [M]:** `owned_groups` passes under LLVM. The hook, the lease's `Drop` and `terminate` are idempotent by design.

**Whole dev profile on LLVM:** measured in §6.4 for cost. It is not needed to fix tests. It would also fix the prod-code hazards in the dev `omp` binary (§3.3 item 5), but it drops Cranelift for iteration.

### 6.3 (c) Cranelift unwinding support on the pinned toolchain: not usable [M/I]

- **[M]** The probe shows no landing pads with the shipped `rustc-codegen-cranelift-preview` component.
- **[I, from upstream source at the pinned commit]** The landing-pad code exists, but it is gated behind the crate feature `unwinding`, with the comment "Not yet included in unstable-features for performance reasons". The rustup component is built without it, so no `-Z` flag or `-Cllvm-args` option enables it.
- **[I, upstream `Readme.md` at the same commit]** "Unwinding on panics ([experimental and not supported on Windows and macOS](https://github.com/rust-lang/rustc_codegen_cranelift/issues/1567), `-Cpanic=abort` is enabled by default)". The team's primary development platform, aarch64 macOS, is explicitly unsupported.
- Using it would therefore mean building and distributing a custom `rustc_codegen_cranelift` with `--features unwinding`, on Linux only. That is not viable. Revisit when the component ships with the feature enabled and supports macOS.

### 6.4 Build-time and disk measurements [M]

**Method.**
- Every "cold" build used a fresh, empty `CARGO_TARGET_DIR` under the scratch directory. It was deleted with `rm -rf` before each run.
- No sccache: `RUSTC_WRAPPER` and `CARGO_BUILD_RUSTC_WRAPPER` were unset, and no user `~/.cargo/config.toml` existed.
- The page cache was dropped before each run (`echo 3 > /proc/sys/vm/drop_caches`).
- Crates were pre-fetched once with `cargo fetch --locked`, so network time is excluded.
- `vendor/python` was prepared once with `just setup-python`.
- Each variant was selected **only** with `--config` on the command line. No file was edited.
- Cranelift and LLVM runs were interleaved (C, L, C, L, C, L) to spread drift.
- Wall time was taken with `date +%s.%N` around the cargo command.
- Host: see the header (4 vCPUs).
- **Noise [I]:** read-only analysis agents (ripgrep, GitHub API downloads) ran at the same time as the first three `omp` builds.

**Cold `cargo build -p omp-app --bin omp --locked`.** This compiles exactly what `cargo run --bin omp` compiles; running the binary is not part of the gate. 967 units each time.

| Variant | Run 1 | Run 2 | Run 3 | Mean ± sd | Target dir |
|---|---:|---:|---:|---:|---:|
| Current config (members on Cranelift) | 419.7 s | 397.1 s | 400.4 s | **405.8 ± 12.2 s** | 5.1 GB |
| `--config 'profile.dev.codegen-backend="llvm"'` (all LLVM) | 417.4 s | 407.5 s | 406.8 s | **410.6 ± 5.9 s** | 5.0 GB |

`[profile.test] codegen-backend = "llvm"` does not touch this build at all: the `dev` profile is unchanged. Its effect on the gate is **zero by construction**.

**Warm rebuild of `omp` after a one-line comment edit to `crates/core/src/lib.rs`.** 41 units rebuild each time; the file was restored afterwards.

| Variant | Run 1 | Run 2 | Run 3 | Mean |
|---|---:|---:|---:|---:|
| Current config | 233.7 s | 232.6 s | 233.9 s | **233.4 s** |
| All-LLVM dev | 236.2 s | 238.3 s | 239.7 s | **238.1 s** (+2.0%) |

**Cold `cargo nextest run -p omp-e2e --tests --no-run --locked`** (the `just e2e-build` recipe). One run per variant, 968 units:

| Variant | Time | Target dir |
|---|---:|---:|
| Current config | 427.9 s | **15 GB** |
| `omp-e2e` alone on LLVM (the per-crate override) | 431.9 s | 15 GB |
| `--config 'profile.test.codegen-backend="llvm"'` | 427.6 s | **7.3 GB** |

**Workspace test build under the LLVM test profile:** cold `cargo nextest run --workspace --exclude omp-e2e --no-run` took 656 s and produced an 18 GB target dir. A Cranelift baseline for the same build **could not be measured on this host**: the disk allowance could not hold it. For comparison, Cranelift test binaries for only `omp-envd` and `omp-vcs` already took 16 GB.

Per-binary sizes (same sources, same profile):

| Test binary | Cranelift | LLVM |
|---|---:|---:|
| `omp-py` probe test | 161 MB | 66 MB |
| `omp-session` probe test | 106 MB | 16 MB |

**First test build after a warm dev `omp` build** (`omp-e2e --tests --no-run`):

Method: a fresh target dir; one `cargo build -p omp-app --bin omp --locked`; then, timed, `cargo nextest run -p omp-e2e --tests --no-run --locked`. The target dir was deleted between variants. One run per variant.

| Variant | Units compiled | Time | Target dir afterwards (dev plus test) |
|---|---:|---:|---:|
| Current config (test profile inherits dev: member libs are reused) | 17 | **177.4 s** | 17 GB |
| `--config 'profile.test.codegen-backend="llvm"'` (member libs compiled a second time, with LLVM) | 51 | **258.8 s** (+81 s, +46%) | 9.8 GB |

This is the real iteration cost of the recommendation **[M]**. When a developer alternates `cargo run` and `just test`, every edited crate and its dependents compile twice: once with Cranelift for dev, once with LLVM for test. Today they compile once. **[I]** For an `omp-core` edit, that is roughly the 41 units of the warm-rebuild measurement compiled again, on top of the test targets. The combined target dir is still smaller (9.8 GB against 17 GB), because Cranelift test binaries are 2–6× larger.

**Reading [I].** On this 4-core x86_64 host, Cranelift does not measurably shorten either the cold `omp` build or a warm rebuild. The time is dominated by LLVM-built dependencies and by linking the statically linked CPython images, which both configurations share. These numbers may differ on the team's many-core aarch64 Macs, where the choice of Cranelift was presumably measured. The recommendation does not depend on this, because it leaves the dev profile alone.

### 6.5 Comparison

| | (a) Hook plus registry | (b) Per-crate LLVM | (b) LLVM for all test builds | (b) All-LLVM dev | (c) Cranelift unwinding |
|---|---|---|---|---|---|
| **Covers isolation hazards in tests** | partly: only resources that have a registry, and only on the owning thread **[M]** | only in the moved crates | **yes [M]** | yes [M] | n/a |
| **Covers cascading hazards in tests** | **no [M]** | only in the moved crates | **yes [M]** | yes [M] | n/a |
| **Covers the dev `omp` binary's prod hazards** | no | partly | no | yes [I] | n/a |
| **Boundary-crash risk** | none added | **reproduced SIGSEGV [M]** | none within test builds [M] | none [I] | n/a |
| **Cold `cargo run --bin omp` gate** | unchanged | unchanged (for `omp-e2e`) | **unchanged by construction [M]** | +1.2%, within noise [M] | n/a |
| **Test build time and disk** | unchanged | unchanged | cold: same time, **about half the disk [M]**; the first test build after a dev build takes **+81 s (+46%) [M]**, because members compile twice | cold: same as the test column; no double compile [I] | n/a |
| **Maintenance** | high: a registry per resource kind, and misses are silent | fragile | one config line; the hook stays as a second layer | one config line; gives up Cranelift for iteration | a custom compiler build, Linux only |
| **Usable now** | yes | no | **yes** | yes | **no [M/I]** |

---

## 7. Recommendation

**Adopt (b) for test builds: compile every test build homogeneously with LLVM, and keep the PR #32 hook as a second layer.**

**Why this option.**
- It is the only option that fixes both hazard classes in tests: every reproduced test hazard in §3.3, the `omp-observability` gaps in §4 and the e2e abort-and-orphan shape in §5 **[M]**.
- It avoids the reproduced mixed-backend SIGSEGV **[M]**.
- It leaves the `cargo run --bin omp` build gate untouched by construction **[M]**.
- Option (a) cannot reach the cascading class **[M]**. Option (c) does not exist on the pinned toolchain for macOS **[M/I]**.

**The cost, accepted knowingly.** Workspace members compile twice, once with Cranelift for dev and once with LLVM for tests. The first test build after a dev build is slower, +81 s (+46%) for `omp-e2e` here **[M]**. Cold test builds are not slower, and they need about half the disk **[M]**.

**Follow-up PR** (a separate change for the repo owner; not part of this audit):

1. **`.cargo/config.toml`**: add the following next to `[profile.dev]`, with a comment that points to this audit and explains why dev and test differ:

   ```toml
   [profile.test]
   # Tests rely on Drop, catch_unwind and JoinError during panics; Cranelift
   # emits no landing pads (docs/audits/cranelift-panic-cleanup.md).
   codegen-backend = "llvm"
   ```

   Also check whether the `omp-ar`, `omp-con` and `omp-observability` package overrides are still needed in the test profile; they become redundant there.
2. **`AGENTS.md`**: record the rule. Test builds are homogeneous LLVM; dev builds stay Cranelift; never add a per-crate backend override for a single test crate (point to the SIGSEGV in §6.2).
3. **Keep** `crates/e2e/src/support/owned_groups.rs`. It still helps when a panic cannot unwind: a panic while panicking, which aborts, or a binary ever built with `panic = "abort"`. It does not help against SIGKILL, for example a nextest timeout, because no hook runs then. It is correct under LLVM **[M]**. Update its module doc: it is no longer the only cleanup path, but defense in depth.
4. **Rewrite the `crates/core/src/append_vec.rs:1336–1361` tests** so their assertions run: they test on LLVM, and they must not claim that landing pads are missing.
5. **Add regression tests**, which become meaningful under LLVM:
   - builtin panic containment through `run_utility` (`crates/shell-builtins/src/host.rs:944`), exit 1 with `internal error`;
   - `TelemetryConfig::estimate_cost` called from another crate with a panicking estimator.
6. **CI**: no workflow change should be needed, since `cargo nextest` and `cargo test --doc` build with the test profile **[I]**. Verify `cargo test --doc` under the new profile, since doctests were not run in this audit.

**Decision left to the owner.** Should the dev `omp` binary also move to LLVM? That would fix the prod-path hazards in §3.3 item 5, where the dev `omp` aborts or hangs on panics that release builds contain. On this host it costs +1.2% cold and +2.0% warm **[M]**, and it would also remove the double compile described above. That needs a measurement on the team's aarch64 Macs before it is decided. It could be adopted as an ADR (`docs/adr/`) by the repo owner, and this audit does not write one.

---

## Appendix A: every individually classified site

644 rows, sorted by path and line. Scope `prod` means the code is compiled into the dev `omp` binary; release builds are LLVM.

| Site | Mechanism | Scope | Class | Sev. | What the skipped cleanup / catch protects |
|---|---|---|---|---|---|
| `crates/agent/src/approvals.rs:562` | I impl Drop | prod | harmless | low | Removes pending approval ticket from in-process map |
| `crates/agent/src/dispatch.rs:1032` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | Tool task panic should become DispatchError::Join; on omp multi-thread runtime it aborts the session |
| `crates/agent/src/hooks.rs:692` | I impl Drop | prod | harmless | low | Removes pending hook reply entry from in-process map |
| `crates/agent/src/loop.rs:67` | I impl Drop | prod | harmless | low | Clears in-process turn-active flag |
| `crates/agent/src/registry.rs:62` | I impl Drop | prod | harmless | low | Decrements reply-obligation counter and notifies; in-process only, process dies on panic |
| `crates/ai/src/account/refresh.rs:406` | I impl Drop | prod | harmless | low | removes refresh flight and notifies waiters; in-process (moot since panic aborts/ends process) |
| `crates/ai/src/answer.rs:108` | I impl Drop | prod | harmless | low | closes chat stream control channel |
| `crates/ai/src/answer.rs:870` | I impl Drop | prod | harmless | low | sends Close to realtime session channel |
| `crates/ai/src/auth/adc.rs:406` | I impl Drop | prod | harmless | low | zeroizes credential strings in memory |
| `crates/ai/src/auth/aws.rs:1275` | J kill_on_drop | prod | harmless | low | kills credential_process on future cancel (not unwind); short-lived same-pgid helper |
| `crates/ai/src/auth/oauth.rs:1870` | M socket bind | test | harmless | low | reserve-then-drop ephemeral port; process-local |
| `crates/ai/src/auth/oauth.rs:2104` | M socket bind | test | harmless | low | occupies ephemeral port to force bind conflict; process-local |
| `crates/ai/src/auth/oauth/callback.rs:117` | I impl Drop | prod | harmless | low | signals callback server shutdown; port bound by this process only |
| `crates/ai/src/auth/oauth/callback.rs:159` | M socket bind | prod | harmless | low | OAuth callback listener; port bound by this process only |
| `crates/ai/src/auth/oauth/callback.rs:163` | M socket bind | prod | harmless | low | OAuth localhost callback listener; process-bound port |
| `crates/ai/src/auth/oauth/callback.rs:166` | M socket bind | prod | harmless | low | IPv6 companion callback listener; process-bound port |
| `crates/ai/src/auth/store.rs:2014` | E std thread | test | cascading | medium | 8 worker threads with asserts; failure aborts process, message lost, tempdir with sqlite leaks |
| `crates/ai/src/body.rs:502` | I impl Drop | prod | harmless | low | clears one-shot body active flag |
| `crates/ai/src/body.rs:845` | I impl Drop | prod | harmless | low | clears one-shot body active flag |
| `crates/ai/src/body.rs:1234` | I impl Drop | test | harmless | low | test lease drop counter |
| `crates/ai/src/layer/admission.rs:69` | I impl Drop | prod | harmless | low | decrements admission counter, wakes waiters; in-process |
| `crates/ai/src/layer/admission.rs:106` | I impl Drop | prod | harmless | low | decrements waiting counter; in-process |
| `crates/ai/src/layer/answer.rs:56` | I impl Drop | prod | harmless | low | aborts session on unwind/cancel; in-process |
| `crates/ai/src/local/applefm.rs:37` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | JoinError import; panic-to-JoinError path for Apple FM spawn_blocking never taken (hang instead) |
| `crates/ai/src/local/applefm.rs:400` | F spawn_blocking | prod | cascading | medium | Apple FM availability spawn_blocking; panic hangs availability() forever instead of JoinError (macOS) |
| `crates/ai/src/local/applefm.rs:522` | I impl Drop | prod | harmless | low | cancels Apple FM generation token; in-process |
| `crates/ai/src/local/applefm.rs:808` | F spawn_blocking | prod | cascading | medium | discovery availability probe spawn_blocking; panic hangs discovery instead of error (macOS) |
| `crates/ai/src/local/applefm.rs:1457` | F spawn_blocking | prod | cascading | medium | Apple FM generate spawn_blocking; panic hangs unless cancel/timeout select arm fires (macOS) |
| `crates/ai/src/local/applefm.rs:1532` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | join_error mapping dead for panics: panicking spawn_blocking hangs awaiting caller forever (macOS) |
| `crates/ai/src/local/applefm/platform.rs:5` | A catch_unwind | prod | cascading | medium | import for extern "C" Swift callback catch; catch fails so panic crosses FFI boundary -> abort (macOS only) |
| `crates/ai/src/local/applefm/platform.rs:308` | I impl Drop | prod | harmless | low | releases Swift string; frees memory |
| `crates/ai/src/local/applefm/platform.rs:420` | I impl Drop | prod | harmless | low | destroys and deallocates Swift value; frees memory |
| `crates/ai/src/local/applefm/platform.rs:448` | I impl Drop | prod | harmless | low | releases Swift object reference; frees memory |
| `crates/ai/src/local/applefm/platform.rs:1248` | A catch_unwind | prod | cascading | medium | fail-open catch in extern "C" FM completion callback no longer catches; panic in advance() aborts process (macOS) |
| `crates/ai/src/local/runtime.rs:147` | I impl Drop | prod | harmless | low | returns reserved bytes to memory pool counter |
| `crates/ai/src/local/runtime.rs:205` | I impl Drop | prod | harmless | low | decrements admission counter |
| `crates/ai/src/operation/job.rs:285` | I impl Drop | prod | harmless | low | sends Cancel to job channel |
| `crates/ai/src/realtime/live.rs:293` | I impl Drop | prod | harmless | low | stops capture/playback and releases leases; in-process |
| `crates/ai/src/session/conversation.rs:660` | I impl Drop | prod | harmless | low | removes draft from in-memory conversation state |
| `crates/ai/src/session/mod.rs:1732` | E std thread | test | cascading | low | commit().unwrap() on std thread; failure aborts test process, join().unwrap never sees Err |
| `crates/ai/src/session/mod.rs:1737` | E std thread | test | cascading | low | commit().unwrap() on std thread; failure aborts test process, join().unwrap never sees Err |
| `crates/ai/src/staging.rs:1764` | I impl Drop | prod | isolation | low | drops encrypted gate spool TempPath; leftover uniquely named omp-llm-gate-* file in temp dir |
| `crates/ai/src/transport/cassette.rs:317` | I impl Drop | prod | harmless | low | pushes capture record to in-memory cassette log |
| `crates/ai/src/transport/cassette.rs:930` | I impl Drop | prod | harmless | low | sets realtime closed flag |
| `crates/ai/src/transport/cassette.rs:1234` | I impl Drop | prod | harmless | low | cancels cassette stream token |
| `crates/ai/src/transport/http.rs:2058` | I impl Drop | prod | harmless | low | cancels in-flight HTTP request token |
| `crates/ai/src/transport/tests.rs:229` | M socket bind | test | harmless | low | ephemeral loopback port fixture |
| `crates/ai/src/transport/tests.rs:1159` | M socket bind | test | harmless | low | ephemeral loopback port fixture |
| `crates/ai/src/transport/tests.rs:1201` | M socket bind | test | harmless | low | ephemeral loopback port fixture |
| `crates/ai/src/transport/tests.rs:1251` | M socket bind | test | harmless | low | ephemeral loopback port fixture |
| `crates/ai/src/transport/tests.rs:1331` | M socket bind | test | harmless | low | ephemeral loopback port fixture |
| `crates/ai/src/transport/tests.rs:1397` | M socket bind | test | harmless | low | ephemeral loopback port fixture |
| `crates/ai/src/transport/tests.rs:1435` | M socket bind | test | harmless | low | ephemeral loopback port fixture |
| `crates/ai/src/transport/tests.rs:1461` | M socket bind | test | harmless | low | ephemeral loopback port fixture |
| `crates/ai/src/transport/websocket_transport.rs:958` | I impl Drop | prod | harmless | low | cancels websocket token |
| `crates/ai/src/transport/websocket_transport.rs:965` | I impl Drop | prod | harmless | low | sets websocket closed flag |
| `crates/app/src/auth_cli.rs:97` | F spawn_blocking | prod | cascading | low | Auth prompt read (termios toggle); panic hangs auth login instead of JoinError |
| `crates/app/src/auth_cli.rs:152` | L terminal/termios | prod | isolation | low | Disables echo; restore is explicit (not Drop), unaffected by Cranelift, only interrupt leaves echo off |
| `crates/app/src/auth_cli.rs:160` | L terminal/termios | prod | harmless | low | Explicit echo restore; no guard involved, read_line does not panic |
| `crates/app/src/chat_control.rs:1960` | F spawn_blocking | prod | cascading | low | Detached git workbench op; panic means Outcome::Git never posted, UI waits |
| `crates/app/src/chat_services/extension_ui.rs:982` | F spawn_blocking | test | cascading | medium | Test dialog driver with panic!/assert; panic hangs awaiting handle until nextest timeout |
| `crates/app/src/chat_services/extension_ui.rs:1105` | F spawn_blocking | test | cascading | medium | Test host driver with expect; panic hangs tokio::join until nextest timeout |
| `crates/app/src/chat_services/misc.rs:314` | E std thread | prod | cascading | medium | Cleanse run thread: panic aborts omp chat, orphaning cleanse checker children (kill_on_drop skipped) |
| `crates/app/src/chat_services/plugins.rs:134` | E std thread | prod | cascading | medium | scope join map_err("marketplace fetch panicked") fail-open never taken; fetch panic aborts omp |
| `crates/app/src/chat_services/stats.rs:41` | F spawn_blocking | prod | cascading | low | Detached stats sync; panic leaves tx undropped, stats panel pending forever |
| `crates/app/src/chat_voice.rs:161` | D JoinError/is_panic/resume_unwind | prod | cascading | low | STT Worker JoinError never produced: spawn_blocking panic hangs await instead (local-stt only) |
| `crates/app/src/chat_voice.rs:296` | I impl Drop | prod | harmless | low | Cancels STT, stops mic capture, aborts task; released at process exit |
| `crates/app/src/chat_voice.rs:332` | I impl Drop | prod | harmless | low | Sends Close to live voice transport task; in-process |
| `crates/app/src/chat_voice.rs:737` | I impl Drop | prod | harmless | low | Cancels STT and closes live runtime; in-process |
| `crates/app/src/chat_voice.rs:833` | F spawn_blocking | prod | cascading | low | STT adapter load; panic hangs run_stt, UI stuck (local-stt only) |
| `crates/app/src/chat_voice.rs:1073` | F spawn_blocking | prod | cascading | medium | STT decode loop; panic hangs await, mic capture never stopped, UI stuck Recording |
| `crates/app/src/chat_voice.rs:1377` | M socket bind | prod | harmless | low | Ephemeral UDP route probe socket; in-process |
| `crates/app/src/daemon.rs:36` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | Daemon rpc/token tasks on multi-thread runtime: panic aborts daemon, JoinError path unreachable |
| `crates/app/src/daemon.rs:160` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | RpcTask error never produced; panic aborts daemon, skips UDS socket removal in finish_shutdown |
| `crates/app/src/daemon.rs:367` | M socket bind | prod | harmless | low | Daemon TCP listener; port freed at process exit |
| `crates/app/src/ext_cli/mod.rs:2937` | J kill_on_drop | prod | isolation | low | Kills uv pip install; orphan plus leftover staging/.wheel files in artifacts dir |
| `crates/app/src/ext_cli/service.rs:630` | F spawn_blocking | prod | cascading | low | git head_sha lookup after clone; panic hangs plugin install instead of JoinError |
| `crates/app/src/gc_cmd.rs:27` | F spawn_blocking | prod | cascading | medium | omp gc worker; panic hangs command forever (ctrl-c path also awaits it) |
| `crates/app/src/gui.rs:133` | M socket bind | prod | isolation | low | GUI debug socket at env path; stale file on abort, removed before next bind |
| `crates/app/src/gui.rs:135` | E std thread | prod | cascading | low | GUI debug socket accept thread (env-gated); client-handler panic aborts omp |
| `crates/app/src/gui.rs:156` | I impl Drop | prod | isolation | low | Removes GUI debug unix socket; stale file left, but next bind removes it first |
| `crates/app/src/gui.rs:710` | I impl Drop | prod | harmless | low | Records Ok result if unset; in-process state only |
| `crates/app/src/live_path.rs:55` | I impl Drop | prod | harmless | low | Signals live-path monitor thread shutdown; in-process |
| `crates/app/src/live_path.rs:106` | E std thread | prod | cascading | low | macOS Network.framework monitor thread; any panic aborts omp |
| `crates/app/src/live_path.rs:239` | E std thread | prod | harmless | low | Non-macOS stub thread only waits on shutdown recv; cannot panic |
| `crates/app/src/live_reachability.rs:296` | M socket bind | prod | harmless | low | Ephemeral UDP reachability probe socket; in-process |
| `crates/app/src/main.rs:54` | B panic hook | prod | harmless | low | Hook only logs/prints panic; still runs before abort/unwind under Cranelift |
| `crates/app/src/profile_alias.rs:236` | E std thread | test | cascading | low | Concurrent alias install thread unwrap; panic aborts instead of join Err |
| `crates/app/src/profile_alias.rs:240` | E std thread | test | cascading | low | Concurrent alias install thread unwrap; panic aborts instead of join Err |
| `crates/app/src/rpc_mode.rs:130` | I impl Drop | prod | harmless | low | Removes pending RPC UI reply entry from in-process map |
| `crates/app/src/startup_notice.rs:38` | E std thread | prod | harmless | low | Startup watchdog thread only eprintln loop; effectively cannot panic |
| `crates/app/src/startup_update.rs:70` | I impl Drop | prod | harmless | low | Aborts startup update-check task; in-process |
| `crates/app/src/startup_update.rs:262` | I impl Drop | test | harmless | low | Test sentinel sends on channel when aborted task drops; abort not panic path |
| `crates/app/src/update_cmd.rs:201` | I impl Drop | prod | isolation | medium | Removes fixed cache update.lock; skipped leaves stale lock blocking future omp updates |
| `crates/app/tests/config.rs:369` | E std thread | test | cascading | low | Config updater thread expect; panic aborts instead of join Err |
| `crates/app/tests/config.rs:376` | E std thread | test | cascading | low | Config updater thread expect; panic aborts instead of join Err |
| `crates/app/tests/env_process_protocol.rs:49` | M socket bind | test | harmless | low | Port-0 readiness listener; in-process (test's looping child is separate, see notes) |
| `crates/app/tests/it/envd_contract.rs:151` | I impl Drop | test | harmless | low | Removes lease marker file inside test tempdir |
| `crates/app/tests/it/envd_contract.rs:679` | I impl Drop | test | harmless | low | Aborts in-process server/extension tasks |
| `crates/app/tests/it/envd_documents.rs:256` | E std thread | test | cascading | low | Walker thread expect; panic aborts test process instead of join Err |
| `crates/audio/src/audio.rs:145` | I impl Drop | prod | harmless | low | marks playback stopped and zeroes level; runs on audio thread whose panic aborts anyway |
| `crates/audio/src/audio.rs:376` | I impl Drop | prod | harmless | low | aborts playback stream; in-process |
| `crates/audio/src/audio.rs:489` | I impl Drop | prod | harmless | low | stops capture stream; in-process |
| `crates/audio/src/coordinator.rs:295` | I impl Drop | prod | harmless | low | releases microphone ownership/TTS state in coordinator; in-process state only |
| `crates/audio/src/coordinator.rs:319` | I impl Drop | prod | harmless | low | restores TTS ducking gain; in-process state |
| `crates/audio/src/coordinator.rs:340` | I impl Drop | prod | harmless | low | releases TTS suspension count; in-process state |
| `crates/audio/src/device.rs:204` | I impl Drop | prod | harmless | low | stops and joins hot-plug watcher thread; in-process only |
| `crates/audio/src/device.rs:221` | E std thread | prod | cascading | medium | device hot-plug watcher thread; panic in native enumeration aborts omp process |
| `crates/audio/src/device/coreaudio.rs:416` | I impl Drop | prod | harmless | low | CFRelease of CoreFoundation string; frees memory |
| `crates/audio/src/device/coreaudio.rs:682` | E std thread | prod | cascading | low | detached CoreAudio queue dispose thread; panic aborts process (macOS) |
| `crates/audio/src/device/coreaudio.rs:695` | I impl Drop | prod | harmless | low | stops/disposes CoreAudio queue; in-process |
| `crates/audio/src/device/coreaudio.rs:788` | E std thread | prod | cascading | low | detached CoreAudio queue dispose thread; panic aborts process (macOS) |
| `crates/audio/src/device/coreaudio.rs:801` | I impl Drop | prod | harmless | low | stops/disposes CoreAudio queue; in-process |
| `crates/audio/src/device/linux.rs:843` | I impl Drop | prod | harmless | low | signals worker-done channel on thread exit; thread panic aborts anyway |
| `crates/audio/src/device/linux.rs:924` | E std thread | prod | cascading | medium | PulseAudio/ALSA playback worker runs fill callback; panic aborts omp process |
| `crates/audio/src/device/linux.rs:1000` | I impl Drop | prod | harmless | low | stops playback worker thread and closes PCM stream; in-process |
| `crates/audio/src/device/linux.rs:1033` | E std thread | prod | cascading | medium | PulseAudio/ALSA capture worker runs sink callback; panic aborts omp process |
| `crates/audio/src/device/linux.rs:1109` | I impl Drop | prod | harmless | low | stops capture worker thread and closes PCM stream; in-process |
| `crates/audio/src/device/wasapi.rs:267` | I impl Drop | prod | harmless | low | COM Release; frees reference |
| `crates/audio/src/device/wasapi.rs:308` | I impl Drop | prod | harmless | low | CloseHandle on event; fd-like, process-local |
| `crates/audio/src/device/wasapi.rs:327` | I impl Drop | prod | harmless | low | CoUninitialize on worker thread; process-local |
| `crates/audio/src/device/wasapi.rs:674` | I impl Drop | prod | harmless | low | stops WASAPI playback stream; process-local |
| `crates/audio/src/device/wasapi.rs:710` | I impl Drop | prod | harmless | low | stops WASAPI capture stream; process-local |
| `crates/audio/src/device/wasapi.rs:731` | E std thread | prod | cascading | medium | WASAPI playback thread; "panicked during startup" Err branch unreachable, abort instead (Windows) |
| `crates/audio/src/device/wasapi.rs:756` | I impl Drop | prod | harmless | low | stops and joins WASAPI playback thread |
| `crates/audio/src/device/wasapi.rs:775` | E std thread | prod | cascading | medium | WASAPI capture thread; "panicked during startup" Err branch unreachable, abort instead (Windows) |
| `crates/audio/src/device/wasapi.rs:800` | I impl Drop | prod | harmless | low | stops and joins WASAPI capture thread |
| `crates/cache/src/secret_key.rs:242` | E std thread | test | cascading | low | 8 key creators with expect; panic aborts process instead of join Err; tempdir leaked |
| `crates/cache/src/telemetry_cache.rs:175` | I impl Drop | prod | harmless | low | Sets telemetry query cancelled flag; in-process |
| `crates/chat/src/autocomplete/files.rs:70` | E std thread | prod | cascading | low | File-index walker thread; a walker panic aborts omp chat |
| `crates/chat/src/commands/misc.rs:334` | E std thread | prod | harmless | low | Relay thread forwards cleanse result; no panicking code |
| `crates/chat/src/editor.rs:271` | I impl Drop | prod | isolation | low | Removes uniquely named editor draft in temp dir; leaked on panic |
| `crates/chat/src/gitwatch.rs:92` | I impl Drop | prod | harmless | low | Aborts git watch task; in-process |
| `crates/chat/src/gitwatch.rs:169` | F spawn_blocking | prod | cascading | low | let-else relies on JoinError to stop watcher; probe panic hangs git watch loop |
| `crates/chat/src/gitwatch.rs:238` | J kill_on_drop | prod | isolation | low | Kills gh pr view child; short-lived orphan |
| `crates/chat/src/gitwatch.rs:267` | H multi-thread runtime | test | cascading | low | GitWatch loop spawned on MT worker; worker panic aborts test process |
| `crates/chat/src/gitwatch.rs:303` | H multi-thread runtime | test | cascading | low | GitWatch loop spawned on MT worker; worker panic aborts test process |
| `crates/chat/src/host.rs:4687` | I impl Drop | prod | harmless | low | Sends Cancel/Quit to controller; in-process channels, no tty restore here |
| `crates/chat/src/host.rs:5239` | I impl Drop | prod | harmless | low | Sends Cancel/Quit to controller; in-process channels |
| `crates/chat/src/notices/voice.rs:664` | E std thread | prod | cascading | low | Vocalizer fallback runtime thread (no ambient runtime); worker panic aborts process |
| `crates/chat/src/notices/voice.rs:885` | I impl Drop | prod | harmless | low | Marks vocalizer closed and clears queue; in-process audio |
| `crates/chat/tests/keys.rs:222` | E std thread | test | harmless | low | Fake kernel responder thread only recv/send; cannot panic |
| `crates/core/src/append_vec.rs:305` | I impl Drop | prod | harmless | low | frees bucket memory |
| `crates/core/src/append_vec.rs:790` | I impl Drop | prod | harmless | low | drops elements (clear); frees memory |
| `crates/core/src/append_vec.rs:1211` | E std thread | test | cascading | low | scoped push threads; AppendVec bug panic aborts test process instead of clean failure |
| `crates/core/src/append_vec.rs:1316` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/append_vec.rs:1331` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/append_vec.rs:1338` | C should_panic | test | cascading | low | should_panic passes but relies on inner catch_unwind that no longer catches; mid-test assertions never run |
| `crates/core/src/append_vec.rs:1341` | A catch_unwind | test | cascading | low | catch_unwind doesn't catch; panic reaches should_panic early, prefix-publish assertions silently skipped (vacuous pass) |
| `crates/core/src/append_vec.rs:1349` | D JoinError/is_panic/resume_unwind | test | cascading | low | resume_unwind unreachable: catch_unwind above never returns Err; test passes via original panic |
| `crates/core/src/append_vec.rs:1353` | C should_panic | test | harmless | low | should_panic works; test explicitly avoids in-test catch_unwind |
| `crates/core/src/append_vec.rs:1432` | I impl Drop | test | harmless | low | test destructor counter; memory only |
| `crates/core/src/append_vec.rs:1472` | I impl Drop | test | harmless | low | test drop counter; memory only |
| `crates/core/src/append_vec.rs:1774` | E std thread | test | cascading | low | scoped grow threads; AppendVec bug panic aborts test process instead of clean failure |
| `crates/core/src/append_vec.rs:1902` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/cow_bytes.rs:1036` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/cow_bytes.rs:1044` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/cow_bytes.rs:1164` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/encoding/base_n.rs:1047` | I impl Drop | prod | harmless | low | flushes buffered encoder to inner writer; in-process |
| `crates/core/src/encoding/base_n.rs:1214` | I impl Drop | prod | harmless | low | flushes buffered decoder to inner writer; in-process |
| `crates/core/src/open.rs:30` | E std thread | prod | cascading | low | reaper thread for external opener; only logs, panic unlikely but would abort omp |
| `crates/core/src/secret.rs:118` | I impl Drop | prod | harmless | low | zeroizes secret memory; process memory only |
| `crates/core/src/secret.rs:173` | I impl Drop | prod | harmless | low | zeroizes secret bytes; process memory only |
| `crates/core/src/slopjson/incoming.rs:242` | I impl Drop | prod | harmless | low | closes feed as Aborted and wakes readers; in-process |
| `crates/core/src/str.rs:1316` | I impl Drop | prod | harmless | low | drops Arc-backed string storage; frees memory |
| `crates/core/src/str.rs:3823` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/str.rs:3830` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/str.rs:3915` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/str.rs:3926` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/str.rs:3933` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/str.rs:3940` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/src/str.rs:3947` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/tests/sparse.rs:242` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/tests/sparse_container.rs:491` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/tests/sparse_container.rs:552` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/tests/sparse_container.rs:809` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/tests/sparse_container.rs:816` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/core/tests/sparse_container.rs:830` | C should_panic | test | harmless | low | should_panic works under libtest; no cleanup involved |
| `crates/desktop/src/lib.rs:84` | I impl Drop | prod | harmless | low | sets operation cancelled flag |
| `crates/desktop/src/lib.rs:103` | I impl Drop | prod | harmless | low | clears thread-local active operation |
| `crates/desktop/src/lib.rs:730` | E std thread | prod | cascading | high | omp-desktop-session worker; its catch_unwind fails so any worker panic aborts omp |
| `crates/desktop/src/lib.rs:741` | A catch_unwind | prod | cascading | high | desktop worker panic meant to become DesktopError; uncaught on std thread -> whole omp process aborts |
| `crates/desktop/src/lib.rs:814` | I impl Drop | prod | harmless | low | sends Close to worker thread; in-process |
| `crates/desktop/src/lib.rs:1133` | F spawn_blocking | prod | cascading | medium | desktop close spawn_blocking; panic hangs close() instead of "close task failed" |
| `crates/desktop/src/lib.rs:1150` | F spawn_blocking | prod | cascading | medium | desktop operation spawn_blocking; decode/call panic hangs caller instead of JoinError |
| `crates/desktop/src/linux/actor.rs:53` | E std thread | prod | cascading | medium | global Linux desktop actor thread; panic aborts omp process |
| `crates/desktop/src/linux/wayland/libei.rs:486` | I impl Drop | prod | harmless | low | sends LibeiClose to desktop actor; in-process |
| `crates/desktop/src/linux/wayland/mod.rs:277` | M socket bind | test | isolation | low | socket at temp_dir/omp-libei-test-{pid} (fixed per pid); cleanup and LIBEI_SOCKET restore skipped on panic |
| `crates/desktop/src/linux/wayland/mod.rs:282` | E std thread | test | cascading | low | fake libei accept thread panics on listener error -> test process aborts |
| `crates/desktop/src/linux/x11/input.rs:1089` | I impl Drop | prod | isolation | medium | removes XInput MPX master device/ungrabs; skipped leaves extra master pointer in X server |
| `crates/desktop/src/linux/x11/input.rs:1178` | I impl Drop | prod | harmless | low | UI_DEV_DESTROY; kernel destroys uinput device on fd close anyway |
| `crates/desktop/src/win32/input.rs:621` | I impl Drop | prod | isolation | low | restores previous foreground window (desktop-global state, Windows) |
| `crates/driver/src/adw/production.rs:117` | J kill_on_drop | prod | isolation | medium | Kills adw phase binary (arbitrary checks); orphan keeps running/consuming CPU |
| `crates/driver/src/cfg.rs:620` | E std thread | test | cascading | low | Config lock workers unwrap; panic aborts test process instead of join Err |
| `crates/driver/src/cleanse/production.rs:146` | J kill_on_drop | prod | isolation | medium | Kills cleanse checker process; orphan keeps running in project dir |
| `crates/driver/src/ext_updates.rs:148` | I impl Drop | prod | harmless | low | Unlocks advisory lock on ext update state; OS releases it at process exit |
| `crates/driver/src/headless/ask.rs:45` | I impl Drop | prod | harmless | low | Removes presenter ask registration from in-process map |
| `crates/driver/src/headless/kernel.rs:197` | I impl Drop | prod | isolation | low | Removes ephemeral no-session journal dir; skipped leaves private session files in data dir |
| `crates/driver/src/subagent/spawn.rs:1405` | I impl Drop | test | harmless | low | Aborts test hook responder task |
| `crates/driver/src/subagent/workpool_runtime.rs:288` | I impl Drop | prod | harmless | low | Cancels worker spawn token; in-process |
| `crates/driver/src/subagent/workpool_scheduler.rs:806` | I impl Drop | prod | harmless | low | Cancels workpools and releases in-process producer ownership |
| `crates/e2e/src/support/docserver.rs:111` | I impl Drop | test | isolation | low | aborts docserver task, removes socket in unique scratch dir |
| `crates/e2e/src/support/envd.rs:198` | I impl Drop | test | isolation | low | aborts server tasks, removes socket in unique scratch temp dir |
| `crates/e2e/src/support/envd.rs:255` | I impl Drop | test | isolation | medium | drops OwnedProcess (kills envd daemon group) and removes socket; relies on panic hook |
| `crates/e2e/src/support/envd.rs:290` | I impl Drop | test | harmless | low | aborts frame bridge task |
| `crates/e2e/src/support/owned_groups.rs:114` | I impl Drop | test | isolation | high | SIGKILLs leased process group; skipped, covered only by thread-scoped panic hook |
| `crates/e2e/src/support/owned_groups.rs:126` | B panic hook | test | isolation | high | hook kills panicking thread's leased process groups; sole substitute for skipped OwnedProcess/GroupLease Drop |
| `crates/e2e/src/support/owned_groups.rs:127` | B panic hook | test | isolation | high | kills only groups owned by panicking thread; groups owned by other threads orphaned on abort |
| `crates/e2e/src/support/owned_groups.rs:233` | B panic hook | test | harmless | low | child-proof test hook recording registry state; runs in re-executed child process |
| `crates/e2e/src/support/owned_groups.rs:250` | E std thread | test | harmless | low | spawns survivor in child proof; expect-free body besides spawn result |
| `crates/e2e/src/support/process.rs:95` | J kill_on_drop | test | isolation | medium | kill_on_drop skipped; Unix leader killed by group-lease hook only if owner thread panics |
| `crates/e2e/src/support/process.rs:173` | I impl Drop | test | isolation | medium | Windows only: kills child; no panic-hook fallback there, child orphaned |
| `crates/e2e/tests/p1_doc_race.rs:22` | H multi-thread runtime | test | cascading | medium | docserver tasks on 4 workers; server panic aborts process instead of failing test |
| `crates/e2e/tests/p6_crash_resume.rs:242` | E std thread | test | harmless | low | PTY drainer returns errors, never panics |
| `crates/e2e/tests/p6_crash_resume.rs:261` | I impl Drop | test | harmless | low | stops and joins PTY drainer thread |
| `crates/e2e/tests/p6_crash_resume.rs:480` | H multi-thread runtime | test | cascading | high | gateway tasks on workers; abort skips hook for test-thread groups, orphaning omp chats/daemons |
| `crates/e2e/tests/p7_tui.rs:34` | L terminal/termios | test | harmless | low | termios import for PTY assertions |
| `crates/e2e/tests/p7_tui.rs:635` | L terminal/termios | test | harmless | low | PTY baseline termios field; PTY not a real TTY |
| `crates/e2e/tests/p7_tui.rs:653` | E std thread | test | cascading | medium | PTY reader panics on read error: abort, omp chat child (no lease) orphaned |
| `crates/e2e/tests/p7_tui.rs:706` | L terminal/termios | test | harmless | low | reads final PTY termios for assertion |
| `crates/e2e/tests/p7_tui.rs:858` | L terminal/termios | test | harmless | low | assertion helper comparing PTY termios |
| `crates/e2e/tests/p7_tui.rs:925` | H multi-thread runtime | test | cascading | medium | in-process gateway tasks on workers: panic aborts, PTY omp child not reaped |
| `crates/e2e/tests/p7_tui.rs:1118` | H multi-thread runtime | test | cascading | medium | same: worker panic aborts process; chat children lack group lease |
| `crates/e2e/tests/p9_isolation.rs:209` | I impl Drop | test | harmless | low | cancels and aborts in-process serve tasks |
| `crates/env/src/client.rs:708` | I impl Drop | prod | harmless | low | aborts extension bridge task |
| `crates/env/src/client.rs:901` | E std thread | prod | cascading | medium | env client response router thread; any panic aborts the omp process instead of closing client |
| `crates/env/src/client.rs:905` | E std thread | prod | cascading | low | cancellation router thread; panic (unlikely) would abort omp process |
| `crates/env/src/client.rs:907` | E std thread | prod | cascading | low | lease-close router thread; panic (unlikely) would abort omp process |
| `crates/env/src/client.rs:3207` | I impl Drop | prod | harmless | low | finishes stream and queues lease close to env server |
| `crates/env/src/client.rs:3339` | I impl Drop | prod | harmless | low | unregisters request stream from client map |
| `crates/env/src/client.rs:4002` | I impl Drop | prod | harmless | low | cancels and finishes blob download stream |
| `crates/env/src/client.rs:4419` | E std thread | prod | cascading | low | scoped cancel-sender thread; panic (unlikely) would abort omp process |
| `crates/env/src/guard.rs:58` | I impl Drop | prod | harmless | low | queues cancel of env request over channel; server sees disconnect when process dies |
| `crates/env/src/guard.rs:112` | I impl Drop | prod | harmless | low | queues worker termination to in-process supervisor lane |
| `crates/env/src/windows.rs:412` | I impl Drop | prod | harmless | low | closes process token handle |
| `crates/env/src/windows.rs:473` | I impl Drop | prod | harmless | low | closes kernel handle |
| `crates/env/tests/client.rs:131` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:186` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:231` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:440` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:486` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:652` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:822` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:896` | E std thread | test | cascading | medium | server-thread expect (receive timeout) panic aborts test process, losing message |
| `crates/env/tests/client.rs:966` | E std thread | test | cascading | medium | hello server-thread receive().expect panic aborts process; join().expect never sees Err |
| `crates/env/tests/client.rs:982` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:1084` | E std thread | test | cascading | medium | server-thread expect (receive timeout) panic aborts test process, losing message |
| `crates/env/tests/client.rs:1109` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/client.rs:1156` | E std thread | test | cascading | medium | server-thread assert/expect panic aborts test process (SIGABRT), losing assertion message |
| `crates/env/tests/extension_client.rs:45` | M socket bind | test | isolation | low | extension socket in shared /tmp (unique pid+nanos name); remove_file skipped on panic leaves socket file |
| `crates/env/tests/extension_client.rs:102` | M socket bind | test | isolation | low | extension socket in shared /tmp (unique pid+nanos name); remove_file skipped on panic leaves socket file |
| `crates/envd/src/blobs.rs:117` | D JoinError/is_panic/resume_unwind | prod | cascading | low | FinalizeTask JoinError variant; spawn_blocking finalize panic hangs instead of error |
| `crates/envd/src/blobs.rs:387` | F spawn_blocking | prod | cascading | low | retention collector; panic hangs collector loop, stops GC silently |
| `crates/envd/src/blobs.rs:1177` | F spawn_blocking | prod | cascading | low | blob stage finish; panic hangs instead of FinalizeTask |
| `crates/envd/src/browser_daemon.rs:237` | E std thread | prod | cascading | high | entire browser actor on std thread; any webview-handling panic aborts omp |
| `crates/envd/src/browser_daemon.rs:538` | E std thread | prod | cascading | low | relay stderr capture thread; simple loop |
| `crates/envd/src/browser_daemon.rs:1048` | E std thread | prod | cascading | low | SurfaceWatch polling thread; trivial body |
| `crates/envd/src/browser_daemon.rs:1073` | I impl Drop | prod | harmless | low | stops and joins SurfaceWatch thread |
| `crates/envd/src/browser_fetch.rs:52` | E std thread | prod | cascading | medium | browser fetch driver thread; panic aborts omp instead of Unavailable error |
| `crates/envd/src/browser_relay.rs:171` | M socket bind | prod | harmless | low | relay port, freed at process exit |
| `crates/envd/src/browser_relay.rs:195` | E std thread | prod | cascading | medium | relay thread runs current-thread runtime; any task panic escapes block_on and aborts |
| `crates/envd/src/browser_relay.rs:269` | I impl Drop | prod | harmless | low | stops relay thread; port freed at process exit |
| `crates/envd/src/browser_relay.rs:304` | I impl Drop | prod | harmless | low | decrements lease count, may cancel shutdown token |
| `crates/envd/src/browser_relay.rs:3158` | M socket bind | test | harmless | low | ephemeral unused proxy port |
| `crates/envd/src/browser_relay.rs:3316` | M socket bind | test | harmless | low | ephemeral occupied-port fixture |
| `crates/envd/src/computer.rs:226` | I impl Drop | prod | harmless | low | clears in-process active run slot |
| `crates/envd/src/computer.rs:707` | F spawn_blocking | prod | cascading | low | clipboard read; panic hangs instead of ClipboardFailed |
| `crates/envd/src/computer.rs:722` | F spawn_blocking | prod | cascading | low | clipboard write; panic hangs |
| `crates/envd/src/direnv.rs:52` | J kill_on_drop | prod | isolation | low | short direnv export child under timeout |
| `crates/envd/src/docs.rs:246` | I impl Drop | prod | harmless | low | sends lease release frame; daemon releases on disconnect anyway |
| `crates/envd/src/docs.rs:267` | I impl Drop | prod | harmless | low | sends close-document frame; daemon releases on disconnect |
| `crates/envd/src/docs.rs:407` | I impl Drop | prod | harmless | low | clears in-process late-diagnostics maps |
| `crates/envd/src/docs.rs:1434` | I impl Drop | prod | harmless | low | cancels shutdown tokens |
| `crates/envd/src/docs.rs:1452` | I impl Drop | prod | harmless | low | removes pending request, sends cancel frame |
| `crates/envd/src/docserver/actor.rs:27` | D JoinError/is_panic/resume_unwind | prod | cascading | low | JoinError import for spawn_blocking workers; panicking worker hangs awaiting handle instead |
| `crates/envd/src/docserver/actor.rs:460` | F spawn_blocking | prod | cascading | low | resolve_target; panic hangs open |
| `crates/envd/src/docserver/actor.rs:682` | I impl Drop | prod | harmless | low | removes in-process lease, cancels open |
| `crates/envd/src/docserver/actor.rs:810` | I impl Drop | prod | harmless | low | sends Shutdown to document actors |
| `crates/envd/src/docserver/actor.rs:860` | I impl Drop | prod | harmless | low | releases in-process path reservation |
| `crates/envd/src/docserver/actor.rs:981` | E std thread | prod | cascading | low | fallback detached send thread; trivial body, panic unlikely |
| `crates/envd/src/docserver/actor.rs:1703` | F spawn_blocking | prod | cascading | medium | unawaited activation worker; panic means ActivationComplete never sent, document stuck |
| `crates/envd/src/docserver/actor.rs:1779` | F spawn_blocking | prod | cascading | medium | unawaited reload worker; panic leaves reload_in_flight forever |
| `crates/envd/src/docserver/actor.rs:1861` | F spawn_blocking | prod | cascading | medium | unawaited permission worker; completion+reply never sent, caller and actor stuck |
| `crates/envd/src/docserver/actor.rs:2297` | F spawn_blocking | prod | cascading | medium | unawaited commit worker; CommitComplete never sent, transaction hangs |
| `crates/envd/src/docserver/actor.rs:2346` | F spawn_blocking | prod | cascading | medium | unawaited delete-commit worker; transaction hangs |
| `crates/envd/src/docserver/actor.rs:2408` | F spawn_blocking | prod | cascading | medium | unawaited move-commit worker; transaction hangs |
| `crates/envd/src/docserver/actor.rs:2668` | D JoinError/is_panic/resume_unwind | prod | cascading | low | join_error maps worker JoinError to Error::Worker; panic arm unreachable (hang instead) |
| `crates/envd/src/docserver/connection.rs:33` | D JoinError/is_panic/resume_unwind | prod | cascading | low | imports JoinError/JoinSet for request tasks; panic-to-JoinError conversion never happens under Cranelift |
| `crates/envd/src/docserver/connection.rs:84` | I impl Drop | prod | harmless | low | decrements open gate, notifies waiters |
| `crates/envd/src/docserver/connection.rs:126` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | request-handler panic meant to end one connection as Task error; instead aborts whole omp process |
| `crates/envd/src/docserver/connection.rs:586` | D JoinError/is_panic/resume_unwind | prod | cascading | low | writer_result maps writer JoinError; panic arm unreachable, writer panic aborts process |
| `crates/envd/src/docserver/daemon.rs:37` | D JoinError/is_panic/resume_unwind | prod | cascading | low | JoinError import for connection JoinSet; panic-to-JoinError never observed |
| `crates/envd/src/docserver/daemon.rs:450` | M socket bind | prod | isolation | low | document socket file; SocketCleanup skipped, stale probe recovers |
| `crates/envd/src/docserver/daemon.rs:600` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | report_connection logs crashed connection and keeps serving; now one connection panic aborts document daemon |
| `crates/envd/src/docserver/daemon.rs:638` | I impl Drop | prod | isolation | low | removes document socket file; stale socket is probed and replaced |
| `crates/envd/src/docserver/daemon.rs:706` | M socket bind | test | harmless | low | socket in TempDir |
| `crates/envd/src/docserver/daemon.rs:723` | M socket bind | test | harmless | low | socket in TempDir, explicitly removed |
| `crates/envd/src/docserver/dap_protocol.rs:171` | J kill_on_drop | prod | isolation | low | stdio DAP adapter; stdin EOF usually ends it |
| `crates/envd/src/docserver/dap_protocol.rs:336` | M socket bind | prod | harmless | low | ephemeral port reservation |
| `crates/envd/src/docserver/dap_protocol.rs:413` | M socket bind | prod | harmless | low | ephemeral port listener |
| `crates/envd/src/docserver/dap_protocol.rs:471` | J kill_on_drop | prod | isolation | medium | socket-mode DAP adapter, stdin null; orphan persists |
| `crates/envd/src/docserver/dap_protocol.rs:498` | I impl Drop | prod | isolation | low | sends Shutdown to DAP adapter writer |
| `crates/envd/src/docserver/dap_session.rs:1390` | J kill_on_drop | prod | isolation | medium | runInTerminal debuggee, stdin null; orphan persists |
| `crates/envd/src/docserver/dap_session.rs:1469` | I impl Drop | prod | isolation | low | removes DAP unix socket file |
| `crates/envd/src/docserver/environment.rs:81` | I impl Drop | prod | harmless | low | releases in-process workspace mutation counts |
| `crates/envd/src/docserver/environment.rs:444` | I impl Drop | prod | harmless | low | releases in-process workspace leases |
| `crates/envd/src/docserver/error.rs:182` | D JoinError/is_panic/resume_unwind | prod | cascading | low | Worker JoinError variant; worker panic now hangs/aborts instead of surfacing document worker failed |
| `crates/envd/src/docserver/fs.rs:392` | I impl Drop | prod | isolation | low | removes uncommitted temp file beside target in workspace |
| `crates/envd/src/docserver/lsp.rs:621` | I impl Drop | prod | harmless | low | decrements in-process activity counter |
| `crates/envd/src/docserver/lsp_process.rs:399` | J kill_on_drop | prod | isolation | low | LSP server child; stdin EOF usually ends it |
| `crates/envd/src/docserver/lsp_process.rs:590` | I impl Drop | prod | isolation | low | kills LSP child, aborts tasks; stdin EOF usually ends server |
| `crates/envd/src/docserver/lsp_registry.rs:659` | I impl Drop | prod | harmless | low | removes publication gate (in-process lock state) |
| `crates/envd/src/docserver/lsp_registry.rs:3023` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | warmup task panic meant to become per-binding WarmupTask error; instead aborts process |
| `crates/envd/src/docserver/summary.rs:329` | F spawn_blocking | prod | cascading | medium | parser panic meant to fall back to ParserFailure; now hangs until cancellation |
| `crates/envd/src/docserver/transaction.rs:1766` | F spawn_blocking | prod | cascading | medium | prepare_move; panic hangs transaction instead of join_failure |
| `crates/envd/src/docserver/transaction.rs:1791` | F spawn_blocking | prod | cascading | medium | prepare_delete; panic hangs transaction instead of join_failure |
| `crates/envd/src/docserver/transaction.rs:1807` | F spawn_blocking | prod | cascading | medium | prepare_write; panic hangs transaction instead of join_failure |
| `crates/envd/src/docserver/transaction.rs:2146` | D JoinError/is_panic/resume_unwind | prod | cascading | low | join_failure for spawn_blocking fs prep; panicking closure hangs transaction instead of precondition failure |
| `crates/envd/src/docserver/types.rs:602` | I impl Drop | prod | harmless | low | flock unlock; kernel releases on exit |
| `crates/envd/src/docserver/watch.rs:136` | I impl Drop | prod | harmless | low | deactivates in-process file watch |
| `crates/envd/src/docserver/windows.rs:27` | D JoinError/is_panic/resume_unwind | prod | cascading | low | Windows task JoinError variant; panicking connection task aborts process (multi-thread runtime) so panic arm unreachable |
| `crates/envd/src/eval/bridge.rs:573` | I impl Drop | prod | harmless | low | unregisters in-process bridge grant |
| `crates/envd/src/eval/bridge.rs:929` | I impl Drop | prod | harmless | low | releases in-process parent binding |
| `crates/envd/src/eval/process.rs:606` | J kill_on_drop | prod | isolation | low | eval child; parent watchdog kills group on reparent |
| `crates/envd/src/eval/process.rs:1068` | I impl Drop | prod | isolation | low | SIGKILLs eval child process group; child watchdog mitigates |
| `crates/envd/src/eval/process.rs:1370` | E std thread | prod | cascading | low | eval child fd-capture reader thread; panic aborts eval child |
| `crates/envd/src/eval/process.rs:1439` | E std thread | prod | cascading | low | eval child parent-watchdog thread; trivial loop |
| `crates/envd/src/exec.rs:250` | I impl Drop | prod | isolation | medium | cancels exec run (terminates spawned command processes) |
| `crates/envd/src/exec.rs:2157` | I impl Drop | prod | harmless | low | removes in-process starting reservation |
| `crates/envd/src/exec.rs:3002` | F spawn_blocking | prod | cascading | medium | output pump holds sequencer lock; panic hangs exec and wedges sequencer |
| `crates/envd/src/exec.rs:3101` | F spawn_blocking | prod | cascading | medium | exit waiter; panic means exit never reported, exec hangs |
| `crates/envd/src/exec_sandbox.rs:481` | I impl Drop | prod | harmless | low | removes proxy attempt token from in-process map |
| `crates/envd/src/exthost/control.rs:2654` | I impl Drop | prod | harmless | low | cancels queued dispatch / spawns cancel write to ext host |
| `crates/envd/src/exthost/extensions.rs:1790` | E std thread | prod | cascading | medium | thread block_on(control.dispatch); any dispatch panic aborts omp instead of Timeout/Err |
| `crates/envd/src/exthost/params.rs:310` | I impl Drop | prod | harmless | low | removes pending pull from in-process map |
| `crates/envd/src/exthost/params.rs:318` | I impl Drop | prod | harmless | low | cancels token |
| `crates/envd/src/exthost/params.rs:660` | I impl Drop | prod | harmless | low | cancels token |
| `crates/envd/src/exthost/services.rs:313` | I impl Drop | prod | harmless | low | removes pending call, sends cancellation |
| `crates/envd/src/exthost/spawn.rs:600` | J kill_on_drop | prod | isolation | medium | Python extension host child, stdin null; may orphan |
| `crates/envd/src/github.rs:393` | F spawn_blocking | prod | cascading | low | git snapshot; hang only until cancellation |
| `crates/envd/src/github.rs:986` | F spawn_blocking | prod | cascading | low | git snapshot; hang only until cancellation |
| `crates/envd/src/github.rs:1276` | F spawn_blocking | prod | cascading | low | GitRepo::require; panic hangs checkout |
| `crates/envd/src/github.rs:1292` | F spawn_blocking | prod | cascading | medium | finish_checkout_git; panic hangs checkout holding repo write lock |
| `crates/envd/src/github.rs:1356` | F spawn_blocking | prod | cascading | low | GitRepo::require; panic hangs push |
| `crates/envd/src/grep.rs:1530` | I impl Drop | test | isolation | medium | removes /tmp/omp-grep-<pid>-<n>; leak reused with stale contents on pid reuse |
| `crates/envd/src/host_info.rs:143` | J kill_on_drop | prod | isolation | low | short lspci/GPU probe child |
| `crates/envd/src/lib.rs:424` | I impl Drop | prod | harmless | low | aborts presence bridge task |
| `crates/envd/src/lib.rs:697` | I impl Drop | prod | harmless | low | cancels and aborts project tasks |
| `crates/envd/src/lib.rs:2244` | J kill_on_drop | prod | harmless | low | daemon intentionally detached (false); idle-timeout owns lifetime |
| `crates/envd/src/managed_skills.rs:291` | I impl Drop | prod | harmless | low | removes in-process name lock entry |
| `crates/envd/src/mcp/client.rs:257` | I impl Drop | prod | isolation | low | spawns async transport close; skipped close can leave MCP server child |
| `crates/envd/src/mcp/config_store.rs:421` | I impl Drop | prod | isolation | medium | removes .mcp-config.lock dir; no stale recovery, later mutations LockTimeout |
| `crates/envd/src/mcp/config_store.rs:533` | E std thread | test | cascading | low | concurrent mutation threads; panic would abort test process instead of join Err |
| `crates/envd/src/mcp/http.rs:318` | I impl Drop | prod | harmless | low | marks closed, cancels lifecycle token |
| `crates/envd/src/mcp/legacy_sse.rs:80` | I impl Drop | prod | harmless | low | removes pending request from in-process map |
| `crates/envd/src/mcp/legacy_sse.rs:102` | I impl Drop | prod | harmless | low | marks closed, cancels lifecycle token |
| `crates/envd/src/mcp/manager.rs:1916` | F spawn_blocking | prod | cascading | low | cache read; panic hangs mount restore (fail-open to skip defeated) |
| `crates/envd/src/mcp/manager.rs:2230` | F spawn_blocking | prod | harmless | low | fire-and-forget cache invalidation; nothing awaits it |
| `crates/envd/src/mcp/manager.rs:2264` | F spawn_blocking | prod | cascading | medium | holds manager.state lock in closure; panic leaves lock held forever, MCP manager deadlocks |
| `crates/envd/src/mcp/manager.rs:3186` | I impl Drop | prod | harmless | low | cancels manager shutdown token |
| `crates/envd/src/mcp/mod.rs:247` | F spawn_blocking | prod | cascading | low | config load; panic hangs reload instead of InvalidRequest |
| `crates/envd/src/mcp/mod.rs:311` | F spawn_blocking | prod | cascading | medium | config mutation under DirectoryLock; panic hangs request and leaves lock dir |
| `crates/envd/src/mcp/smithery.rs:193` | I impl Drop | prod | harmless | low | zeroizes API key memory |
| `crates/envd/src/mcp/stdio.rs:402` | I impl Drop | prod | harmless | low | removes pending request from in-process map |
| `crates/envd/src/mcp/stdio.rs:420` | I impl Drop | prod | isolation | medium | kills MCP server process group; skipped leaves orphan servers |
| `crates/envd/src/mcp/stdio.rs:468` | J kill_on_drop | prod | isolation | medium | MCP stdio server child; npx/node grandchildren may outlive process |
| `crates/envd/src/media_tts.rs:490` | F spawn_blocking | prod | cascading | medium | third-party TTS synthesis; panic hangs speech request |
| `crates/envd/src/memory.rs:111` | I impl Drop | prod | harmless | low | unregisters in-process memory runtime |
| `crates/envd/src/policy.rs:1911` | I impl Drop | prod | harmless | low | releases in-process quota counters |
| `crates/envd/src/presence.rs:223` | I impl Drop | prod | isolation | low | removes on-disk presence record; stale ones expire by pid liveness |
| `crates/envd/src/resource_materializer.rs:107` | F spawn_blocking | prod | cascading | low | materialize; panic hangs request |
| `crates/envd/src/resource_materializer.rs:121` | F spawn_blocking | prod | cascading | low | release; panic hangs request |
| `crates/envd/src/resource_materializer.rs:134` | F spawn_blocking | prod | cascading | low | background expiry cleanup; hang only stalls that timer task |
| `crates/envd/src/resource_materializer.rs:288` | I impl Drop | prod | isolation | low | writes back local edits and removes lease dirs on disk |
| `crates/envd/src/sandbox_proxy.rs:72` | M socket bind | prod | isolation | low | broker.sock in unique tempdir; leaks dir if drop skipped |
| `crates/envd/src/sandbox_proxy.rs:78` | M socket bind | prod | harmless | low | ephemeral port reservation, dropped immediately |
| `crates/envd/src/sandbox_proxy.rs:96` | M socket bind | prod | harmless | low | ephemeral loopback proxy port |
| `crates/envd/src/sandbox_proxy.rs:146` | I impl Drop | prod | isolation | low | stops proxy listener; tempdir with broker.sock leaks |
| `crates/envd/src/sandbox_proxy.rs:228` | E std thread | prod | cascading | medium | sandbox proxy listener thread; panic aborts omp process |
| `crates/envd/src/sandbox_proxy.rs:240` | E std thread | prod | cascading | medium | per-client proxy thread parsing untrusted sandbox traffic; parser panic aborts omp |
| `crates/envd/src/sandbox_proxy.rs:648` | E std thread | prod | cascading | medium | proxy relay copy thread; panic aborts omp |
| `crates/envd/src/sandbox_proxy.rs:1086` | M socket bind | test | harmless | low | ephemeral loopback port in test |
| `crates/envd/src/sandbox_proxy.rs:1111` | M socket bind | test | harmless | low | ephemeral loopback port in test |
| `crates/envd/src/sandbox_proxy.rs:1113` | E std thread | test | cascading | low | serve_once proxy thread; expect on accept, panic aborts test process |
| `crates/envd/src/sandbox_proxy.rs:1192` | M socket bind | test | harmless | low | ephemeral loopback port in test |
| `crates/envd/src/sandbox_proxy.rs:1194` | E std thread | test | cascading | medium | upstream thread holds real assertions; failure aborts process instead of join Err/FAILED |
| `crates/envd/src/sandbox_proxy.rs:1257` | M socket bind | test | harmless | low | ephemeral loopback port in test |
| `crates/envd/src/sandbox_proxy.rs:1259` | E std thread | test | cascading | medium | upstream thread with read/assert; failure aborts test process |
| `crates/envd/src/sandbox_proxy.rs:1330` | M socket bind | test | harmless | low | ephemeral loopback port in test |
| `crates/envd/src/sandbox_proxy.rs:1334` | E std thread | test | cascading | medium | upstream thread asserts ClientHello; failure aborts test process |
| `crates/envd/src/sandbox_proxy.rs:1365` | M socket bind | test | harmless | low | ephemeral loopback port in test |
| `crates/envd/src/sandbox_proxy.rs:1369` | E std thread | test | cascading | medium | upstream thread asserts ClientHello; failure aborts test process |
| `crates/envd/src/sandbox_proxy.rs:1417` | M socket bind | test | harmless | low | ephemeral loopback port in test |
| `crates/envd/src/sandbox_proxy.rs:1419` | E std thread | test | cascading | medium | upstream thread asserts tunnel closed (panics); failure aborts test process |
| `crates/envd/src/security_scan.rs:304` | F spawn_blocking | prod | cascading | medium | scan; Err(_)->Fault::Storage fail-closed path defeated, operation hangs |
| `crates/envd/src/server.rs:67` | D JoinError/is_panic/resume_unwind | prod | cascading | low | JoinError import for env connection tasks; panic arm unreachable |
| `crates/envd/src/server.rs:363` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | env connection task panic meant to log and continue; aborts whole daemon, dropping all clients |
| `crates/envd/src/server.rs:566` | M socket bind | prod | isolation | low | extension DATA unix socket file; left on disk if not cleaned |
| `crates/envd/src/server.rs:593` | D JoinError/is_panic/resume_unwind | prod | cascading | low | finished_result surfaces authority task JoinError; panic aborts process before this runs |
| `crates/envd/src/server.rs:607` | I impl Drop | prod | harmless | low | cancels document authority shutdown token |
| `crates/envd/src/server.rs:918` | I impl Drop | prod | harmless | low | releases in-process agent control binding |
| `crates/envd/src/server.rs:1602` | I impl Drop | prod | harmless | low | cancels token |
| `crates/envd/src/server.rs:3706` | M socket bind | prod | isolation | low | pid-named staging socket; guarded, stale staging removed next bind |
| `crates/envd/src/server.rs:6011` | F spawn_blocking | prod | cascading | medium | privileged mutation; panic hangs dispatch reply |
| `crates/envd/src/server.rs:9017` | I impl Drop | prod | isolation | medium | cancel_all execs of connection (kills running command processes) |
| `crates/envd/src/server.rs:10527` | F spawn_blocking | prod | cascading | medium | unawaited walk stream; Finished never sent, stream never completes |
| `crates/envd/src/server.rs:10591` | F spawn_blocking | prod | cascading | medium | unawaited search stream; Finished never sent, stream never completes |
| `crates/envd/src/server.rs:12261` | I impl Drop | prod | isolation | low | removes env socket file; stale socket probed and replaced |
| `crates/envd/src/server.rs:13568` | M socket bind | test | harmless | low | socket in test tempdir |
| `crates/envd/src/ssh.rs:29` | D JoinError/is_panic/resume_unwind | prod | cascading | low | JoinError import for forward JoinSet; panic arm unreachable |
| `crates/envd/src/ssh.rs:456` | I impl Drop | prod | harmless | low | cancels forward shutdown token (in-process) |
| `crates/envd/src/ssh.rs:753` | M socket bind | prod | harmless | low | user-chosen forward port, freed at process exit |
| `crates/envd/src/ssh.rs:1124` | D JoinError/is_panic/resume_unwind | prod | cascading | low | forward connection panic meant to be reported via SshError::Join; instead aborts process |
| `crates/envd/src/tool_ast_grep.rs:139` | F spawn_blocking | prod | cascading | low | local root resolve; panic hangs |
| `crates/envd/src/tool_ast_grep.rs:152` | F spawn_blocking | prod | cascading | low | granted root resolve; panic hangs |
| `crates/envd/src/tool_ast_grep.rs:240` | I impl Drop | prod | harmless | low | cancels token |
| `crates/envd/src/tool_ast_grep.rs:580` | I impl Drop | prod | isolation | low | removes .materializing temp file |
| `crates/envd/src/tool_document.rs:1528` | I impl Drop | prod | harmless | low | cancels special-write control |
| `crates/envd/src/tool_document.rs:1571` | F spawn_blocking | prod | cascading | medium | special write worker; panic hangs write, cancel guard never disarmed |
| `crates/envd/src/tool_document.rs:3428` | F spawn_blocking | test | cascading | medium | test closure with expect/assert; failure hangs test until nextest timeout |
| `crates/envd/src/tool_lsp.rs:473` | F spawn_blocking | prod | cascading | low | diagnostic target scan; panic hangs instead of Fault::Server |
| `crates/envd/src/tool_lsp.rs:579` | F spawn_blocking | prod | cascading | low | rename pair scan; panic hangs |
| `crates/envd/src/tool_read_sources.rs:556` | F spawn_blocking | prod | cascading | low | cache get meant to fail open; panic hangs read |
| `crates/envd/src/tool_read_sources.rs:588` | F spawn_blocking | prod | cascading | low | cache put meant to fail open; panic hangs read |
| `crates/envd/src/tool_read_sources/tests.rs:62` | I impl Drop | test | harmless | low | aborts in-process test server task |
| `crates/envd/src/tool_read_sources/tests.rs:71` | M socket bind | test | harmless | low | ephemeral loopback test server port |
| `crates/envd/src/tool_read_sources/tests.rs:324` | M socket bind | test | harmless | low | ephemeral loopback test server port |
| `crates/envd/src/tool_read_sources/tests.rs:361` | M socket bind | test | harmless | low | ephemeral loopback test server port |
| `crates/envd/src/tool_read_sources/tests.rs:402` | I impl Drop | test | harmless | low | signals drop probe channel |
| `crates/envd/src/tool_read_sources/tests.rs:412` | M socket bind | test | harmless | low | ephemeral loopback test server port |
| `crates/envd/src/tool_search.rs:121` | F spawn_blocking | prod | cascading | medium | workspace search worker; panic hangs tool call instead of Workspace fault |
| `crates/envd/src/tool_search.rs:175` | F spawn_blocking | prod | cascading | medium | glob walk worker; panic hangs tool call |
| `crates/envd/src/tool_search.rs:245` | F spawn_blocking | prod | cascading | medium | glob walk worker; panic hangs tool call |
| `crates/envd/src/tool_search.rs:386` | I impl Drop | prod | harmless | low | cancels token |
| `crates/envd/src/tool_search.rs:398` | I impl Drop | prod | harmless | low | sets blocking flag, cancels token |
| `crates/envd/src/tool_search.rs:524` | F spawn_blocking | prod | cascading | medium | archive materialization of untrusted bytes; panic hangs |
| `crates/envd/src/tool_search.rs:2407` | M socket bind | test | harmless | low | ephemeral port test server |
| `crates/envd/src/tool_shell.rs:672` | F spawn_blocking | prod | cascading | low | blob put; panic hangs attachment store instead of Fault::Resource |
| `crates/envd/src/tools.rs:1760` | F spawn_blocking | prod | harmless | low | fire-and-forget telemetry append; nothing awaits |
| `crates/envd/src/tools.rs:5100` | F spawn_blocking | prod | cascading | medium | checkpoint snapshot; panic hangs instead of SnapshotFailed |
| `crates/envd/src/tools.rs:5911` | I impl Drop | test | harmless | low | restores process env vars; process dies anyway |
| `crates/envd/src/vault.rs:679` | J kill_on_drop | prod | isolation | low | short Obsidian CLI child; CliChild also kills tree |
| `crates/envd/src/vault.rs:981` | I impl Drop | prod | isolation | medium | kills Obsidian CLI process tree |
| `crates/envd/src/vault.rs:1186` | I impl Drop | prod | isolation | low | removes uncommitted .omp-vault temp file in vault dir |
| `crates/envd/src/vcs/git/mod.rs:24` | F spawn_blocking | prod | cascading | medium | all git blocking ops; panic hangs unless cancel token given |
| `crates/envd/src/vcs/git/refs.rs:137` | I impl Drop | prod | harmless | low | cancels HEAD watcher token |
| `crates/envd/src/worker.rs:316` | I impl Drop | prod | harmless | low | clears in-process binding slot |
| `crates/envd/src/worker.rs:751` | I impl Drop | prod | harmless | low | sends cancel command, settles authority (in-process) |
| `crates/envd/src/worker.rs:978` | I impl Drop | prod | harmless | low | clears in-process binding slot |
| `crates/envd/src/worker.rs:4084` | F spawn_blocking | prod | cascading | medium | control completion; panic hangs, Crashed abort never emitted |
| `crates/envd/src/worker_pool.rs:969` | I impl Drop | prod | harmless | low | cancels worker request token |
| `crates/envd/src/workspace.rs:375` | E std thread | prod | cascading | medium | thread::scope grep lanes over arbitrary files; panic aborts omp |
| `crates/envd/src/workspace/operations.rs:255` | I impl Drop | prod | isolation | low | removes partially built worktree dir (unique ULID name) |
| `crates/envd/tests/browser_relay_contract.rs:161` | M socket bind | test | harmless | low | ephemeral loopback port, freed at exit |
| `crates/envd/tests/browser_relay_contract.rs:439` | M socket bind | test | harmless | low | ephemeral fake proxy port |
| `crates/envd/tests/browser_relay_contract.rs:446` | E std thread | test | cascading | low | fake proxy accept thread; panic (accept error only) aborts test, may orphan probe child process |
| `crates/envd/tests/browser_relay_contract.rs:494` | M socket bind | test | harmless | low | ephemeral occupied-port fixture |
| `crates/ext/src/resolver.rs:519` | J kill_on_drop | prod | isolation | low | Kills uv resolver child; orphan runs to completion |
| `crates/gui/src/host.rs:1423` | E std thread | prod | cascading | low | GUI clipboard read thread; panic aborts GUI process |
| `crates/gui/src/host.rs:1450` | E std thread | prod | cascading | low | GUI clipboard write thread; panic aborts GUI process |
| `crates/gui/src/host.rs:2189` | E std thread | prod | cascading | low | detached clipboard write thread; panic aborts GUI process |
| `crates/journal/src/blob.rs:1093` | I impl Drop | prod | isolation | low | Removes blob-store temp file; skipped leaves temp in data dir (GC grace reclaims) |
| `crates/memory/src/embedding/supervisor.rs:239` | J kill_on_drop | prod | isolation | low | Kills embedding worker; skipped orphans it (likely exits on stdin EOF) |
| `crates/memory/src/session.rs:192` | E std thread | prod | cascading | medium | Shutdown retention thread: panic aborts omp at exit instead of Disconnected->EmbeddingWorker error |
| `crates/oauth/src/callback.rs:220` | M socket bind | prod | harmless | low | OAuth callback listener; port freed at process exit |
| `crates/oauth/src/callback.rs:224` | M socket bind | prod | harmless | low | OAuth callback IPv4 listener; port freed at process exit |
| `crates/oauth/src/callback.rs:225` | M socket bind | prod | harmless | low | OAuth callback IPv6 companion listener; port freed at process exit |
| `crates/oauth/src/http.rs:234` | M socket bind | test | harmless | low | Port-0 loopback listener for hung-request test |
| `crates/py/src/bindings.rs:145` | H multi-thread runtime | prod | cascading | medium | multi-thread DATA runtime for Python; panicking spawned task aborts Python process |
| `crates/py/src/bindings.rs:161` | E std thread | prod | cascading | medium | scoped thread running block_on; panic aborts Python process instead of resume_unwind |
| `crates/py/src/bindings.rs:163` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | scoped-thread Err branch unreachable; panic in DATA block_on thread aborts Python interpreter |
| `crates/rpc/src/client.rs:728` | J kill_on_drop | prod | isolation | medium | Kills spawned omp RPC child; skipped leaves child omp (tests use this client) |
| `crates/rpc/src/uds.rs:88` | M socket bind | prod | isolation | low | UDS RPC socket file; stale file left, next bind probes and removes it |
| `crates/sandbox/src/backends/landlock.rs:795` | M socket bind | prod | harmless | low | relay listener in sandbox private net namespace; dies with relay helper |
| `crates/sandbox/src/backends/landlock.rs:845` | E std thread | prod | cascading | low | per-connection relay thread in sandbox relay helper; panic aborts relay helper process |
| `crates/sandbox/src/backends/landlock.rs:905` | E std thread | prod | cascading | low | relay copy thread; io::copy only, panic would abort relay helper |
| `crates/sandbox/src/runner.rs:631` | I impl Drop | prod | isolation | low | cleans prepared temp files/dirs (masks); leftover uniquely named temp artifacts |
| `crates/sandbox/src/runner.rs:732` | I impl Drop | prod | isolation | low | cleans prepared sandbox temp resources; leftover uniquely named temp artifacts |
| `crates/sandbox/src/runner.rs:892` | J kill_on_drop | prod | isolation | high | kill_on_drop of sandbox child in its own process group; skipped on panic, orphan outlives omp |
| `crates/sandbox/src/runner.rs:1069` | I impl Drop | prod | isolation | high | SIGKILLs sandboxed child process group (own pgid); skipped leaves orphaned sandbox processes |
| `crates/sandbox/src/runtime/docker.rs:183` | I impl Drop | prod | isolation | high | docker rm -f container and artifact cleanup; skipped leaves running container |
| `crates/sandbox/src/runtime/gvisor.rs:124` | I impl Drop | prod | isolation | high | runsc delete --force container and bundle cleanup; skipped leaves gVisor container/dirs |
| `crates/sandbox/src/runtime/gvisor.rs:741` | I impl Drop | prod | isolation | high | SIGKILLs gVisor child process group; skipped leaves orphaned runsc processes |
| `crates/sandbox/src/runtime/watchdog_macos.rs:132` | I impl Drop | prod | isolation | medium | SIGCONTs a SIGSTOPped process group; skipped leaves stopped orphaned processes (macOS) |
| `crates/sandbox/src/runtime/windows.rs:434` | I impl Drop | prod | harmless | low | FreeSid; frees memory |
| `crates/sandbox/src/runtime/windows.rs:442` | I impl Drop | prod | harmless | low | CloseHandle; process-local |
| `crates/sandbox/src/runtime/windows.rs:465` | I impl Drop | prod | harmless | low | clears registered job handle slot |
| `crates/sandbox/src/runtime/windows.rs:471` | I impl Drop | prod | isolation | low | TerminateJobObject on cancel; KILL_ON_JOB_CLOSE still kills when process exits (Windows) |
| `crates/sandbox/src/runtime/windows.rs:493` | F spawn_blocking | prod | cascading | medium | AppContainer run_blocking; panic hangs sandbox run forever; job not terminated (Windows) |
| `crates/sandbox/src/runtime/windows.rs:844` | I impl Drop | prod | harmless | low | frees proc thread attribute list memory |
| `crates/sandbox/src/runtime/windows.rs:937` | E std thread | prod | cascading | low | AppContainer stdin pump thread; "pump panicked" Err path unreachable (Windows) |
| `crates/sandbox/src/runtime/windows.rs:940` | E std thread | prod | cascading | low | AppContainer stdout pump thread; "pump panicked" Err path unreachable (Windows) |
| `crates/sandbox/src/runtime/windows.rs:947` | E std thread | prod | cascading | low | AppContainer stderr pump thread; "pump panicked" Err path unreachable (Windows) |
| `crates/sandbox/src/runtime/windows.rs:989` | I impl Drop | prod | harmless | low | closes child stdio handles |
| `crates/sandbox/src/runtime/windows.rs:1113` | I impl Drop | prod | harmless | low | FreeLibrary userenv.dll |
| `crates/sandbox/src/runtime/windows_acl.rs:226` | I impl Drop | prod | isolation | high | reverts file ACL mutations for AppContainer; skipped leaves host files with altered ACLs (Windows) |
| `crates/sandbox/tests/bubblewrap.rs:405` | M socket bind | test | isolation | low | socket inside tempdir; TempDir not removed on panic (unique name, disk only) |
| `crates/sandbox/tests/bubblewrap.rs:559` | M socket bind | test | isolation | low | socket inside tempdir; TempDir not removed on panic (unique name, disk only) |
| `crates/sandbox/tests/bubblewrap.rs:612` | E std thread | test | cascading | low | socket server thread expect failure aborts process; bwrap probe child may be left running |
| `crates/sandbox/tests/docker.rs:38` | I impl Drop | test | harmless | low | restores process env vars; process-local, dies with test |
| `crates/sandbox/tests/landlock.rs:53` | M socket bind | test | isolation | low | socket inside tempdir; TempDir not removed on panic (unique name, disk only) |
| `crates/sandbox/tests/seatbelt.rs:206` | M socket bind | test | isolation | low | socket inside tempdir; TempDir not removed on panic (unique name, disk only) |
| `crates/sandbox/tests/seatbelt.rs:515` | M socket bind | test | isolation | low | socket inside tempdir; TempDir not removed on panic (unique name, disk only) |
| `crates/sandbox/tests/seatbelt.rs:543` | M socket bind | test | harmless | low | ephemeral loopback TCP listener |
| `crates/sandbox/tests/seatbelt.rs:593` | M socket bind | test | isolation | low | socket inside tempdir; TempDir not removed on panic (unique name, disk only) |
| `crates/sandbox/tests/seatbelt.rs:614` | M socket bind | test | isolation | low | socket inside tempdir; TempDir not removed on panic (unique name, disk only) |
| `crates/sandbox/tests/seatbelt.rs:625` | M socket bind | test | isolation | low | socket inside tempdir; TempDir not removed on panic (unique name, disk only) |
| `crates/secrets/tests/key_persistence.rs:22` | E std thread | test | cascading | low | Key creator threads expect; panic aborts test process instead of join Err |
| `crates/serve/src/auth.rs:69` | I impl Drop | prod | harmless | low | Cancels in-process auth session for abandoned flow |
| `crates/serve/src/blob.rs:46` | F spawn_blocking | prod | harmless | low | fs::metadata cannot panic; JoinError branch merely dead |
| `crates/serve/src/blob.rs:74` | F spawn_blocking | prod | cascading | low | Blob open_range worker; store panic hangs gRPC call instead of internal Status |
| `crates/serve/src/blob.rs:153` | F spawn_blocking | prod | cascading | low | Blob put worker; store panic hangs gRPC call instead of internal Status |
| `crates/serve/src/blob.rs:175` | F spawn_blocking | prod | harmless | low | fs::remove_file cannot panic; JoinError branch merely dead |
| `crates/serve/src/blob.rs:210` | D JoinError/is_panic/resume_unwind | prod | cascading | low | Maps blob worker JoinError to Status; dead branch, panicking store op hangs the RPC instead |
| `crates/shell-builtins/src/host.rs:31` | A catch_unwind | prod | cascading | high | import for run_caught; generic catch compiled in Cranelift never catches builtin panics |
| `crates/shell-builtins/src/host.rs:231` | I impl Drop | prod | harmless | low | sets cancel flag; normal drops still run, panic path hangs anyway |
| `crates/shell-builtins/src/host.rs:466` | E std thread | prod | harmless | low | child stderr drain thread; body cannot panic |
| `crates/shell-builtins/src/host.rs:689` | E std thread | prod | harmless | low | forward_stderr io::copy thread; body cannot panic |
| `crates/shell-builtins/src/host.rs:991` | F spawn_blocking | prod | cascading | high | every utility builtin runs here; any builtin panic hangs the shell command forever |
| `crates/shell-builtins/src/host.rs:1029` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | JoinError branch for builtin worker never taken; panicking spawn_blocking hangs instead |
| `crates/shell-builtins/src/host.rs:1051` | I impl Drop | prod | cascading | medium | decrements PANIC_SCOPE_DEPTH; leak makes crash hook treat later panics on that thread as recoverable |
| `crates/shell-builtins/src/host.rs:1059` | A catch_unwind | prod | cascading | high | builtin panic containment fails; panic escapes into spawn_blocking so shell command hangs instead of exit 1 |
| `crates/shell-builtins/src/hostname.rs:43` | I impl Drop | prod | harmless | low | Windows WSACleanup; in-process |
| `crates/shell-builtins/src/ifne.rs:143` | E std thread | prod | harmless | low | scoped pipe-drain threads with non-panicking bodies |
| `crates/shell-builtins/src/jq.rs:1060` | I impl Drop | prod | harmless | low | clears thread-local jq runtime |
| `crates/shell-builtins/src/mkdir.rs:297` | I impl Drop | prod | harmless | low | restores process umask; panic already hangs builtin; tests die |
| `crates/shell-builtins/src/proc_snapshot.rs:835` | I impl Drop | prod | harmless | low | Windows: closes process handle |
| `crates/shell-builtins/src/sed.rs:7831` | I impl Drop | prod | harmless | low | restores thread-local sed write policy; thread dies with panic |
| `crates/shell-builtins/src/sed.rs:8799` | C should_panic | test | harmless | low | should_panic on pure index bound; libtest catch works |
| `crates/shell-builtins/src/sort.rs:249` | E std thread | prod | cascading | medium | sort --check reader thread; uutils panic aborts whole omp instead of builtin error |
| `crates/shell-builtins/src/sort.rs:974` | E std thread | prod | cascading | medium | ext_sort sorter thread; panic aborts omp process bypassing run_caught |
| `crates/shell-builtins/src/sort.rs:1573` | E std thread | prod | cascading | medium | merge reader thread; panic aborts omp process bypassing run_caught |
| `crates/shell-builtins/src/support/posix.rs:28` | I impl Drop | test | harmless | low | restores _POSIX2_VERSION env var in-process |
| `crates/shell-builtins/src/tail.rs:2703` | I impl Drop | prod | harmless | low | unix no-op Drop |
| `crates/shell-builtins/src/tail.rs:2771` | I impl Drop | prod | harmless | low | Windows: closes process handle; frees handle |
| `crates/shell-builtins/tests/streaming.rs:27` | E std thread | test | cascading | low | stdout reader expect on read_line; panic aborts test process, omp-sh child may linger |
| `crates/shell-builtins/tests/streaming.rs:100` | E std thread | test | cascading | low | stdout drain thread with expect; panic aborts process instead of FAILED |
| `crates/shell-builtins/tests/streaming.rs:105` | E std thread | test | cascading | low | stderr drain thread with expect; panic aborts process instead of FAILED |
| `crates/shell/src/builtins/exec.rs:122` | J kill_on_drop | prod | isolation | low | exec child killed on drop; panic leaves it running |
| `crates/shell/src/builtins/fc.rs:423` | I impl Drop | prod | isolation | low | removes /tmp/brush-fc-<pid>-N.sh; fixed temp dir, pid-named |
| `crates/shell/src/builtins/terminal.rs:41` | I impl Drop | prod | isolation | medium | restores termios after read -s etc.; panic leaves real TTY echo/raw off |
| `crates/shell/src/commands.rs:523` | F spawn_blocking | prod | cascading | high | builtin in owned (pipeline) shell; panic hangs pipeline instead of ThreadingError |
| `crates/shell/src/env.rs:54` | N scopeguard/defer | prod | harmless | low | scope guard struct; in-memory env scopes |
| `crates/shell/src/env.rs:60` | N scopeguard/defer | prod | harmless | low | pushes env scope; in-memory |
| `crates/shell/src/env.rs:84` | I impl Drop | prod | harmless | low | pops shell env scope; in-memory |
| `crates/shell/src/env.rs:84` | N scopeguard/defer | prod | harmless | low | pops shell env scope; in-memory |
| `crates/shell/src/error.rs:229` | D JoinError/is_panic/resume_unwind | prod | cascading | medium | ThreadingError from JoinError unreachable for panics; tasks hang (blocking) or abort (multi-thread) |
| `crates/shell/src/interp.rs:24` | N scopeguard/defer | prod | harmless | low | import |
| `crates/shell/src/interp.rs:1844` | N scopeguard/defer | prod | harmless | low | command-scoped env; shell state lost on panic anyway |
| `crates/shell/src/interp.rs:2544` | E std thread | prod | harmless | low | heredoc writer thread; write_all result ignored, cannot panic |
| `crates/shell/src/processes.rs:367` | I impl Drop | prod | isolation | medium | kills external child on drop; panic orphans running commands |
| `crates/shell/src/shell.rs:201` | I impl Drop | prod | harmless | low | aborts internal job tasks; in-process |
| `crates/shell/src/shell.rs:728` | I impl Drop | test | harmless | low | test marker flag |
| `crates/shell/src/sys/stubs/async_pipe.rs:24` | F spawn_blocking | prod | harmless | low | non-Unix pipe read_to_string; errors returned, no panic source |
| `crates/shell/src/sys/tokio_process.rs:50` | J kill_on_drop | prod | harmless | low | explicitly disabled; ChildProcess Drop owns termination |
| `crates/shell/src/sys/unix/terminal.rs:6` | L terminal/termios | prod | harmless | low | termios import |
| `crates/shell/src/sys/unix/terminal.rs:15` | L terminal/termios | prod | harmless | low | saved termios field |
| `crates/shell/src/sys/unix/terminal.rs:39` | L terminal/termios | prod | isolation | low | applies termios; restore via AutoModeGuard skipped on panic |
| `crates/shell/src/sys/windows/terminal.rs:31` | I impl Drop | prod | harmless | low | Windows: closes toolhelp snapshot handle |
| `crates/tool/src/incoming.rs:161` | I impl Drop | prod | harmless | low | aborts parser feed; in-memory |
| `crates/tool/src/incoming.rs:185` | I impl Drop | prod | harmless | low | aborts uncommitted feed when last producer drops; in-memory |
| `crates/tool/src/incoming.rs:388` | I impl Drop | prod | harmless | low | releases pull slot flag; in-memory |
| `crates/tools/src/browser.rs:331` | I impl Drop | prod | harmless | low | releases browser sessions for owner on tool drop; not panic path |
| `crates/tools/src/computer.rs:520` | I impl Drop | prod | harmless | low | releases desktop host on tool drop; not on panic path |
| `crates/tools/src/edit/observer.rs:322` | F spawn_blocking | prod | cascading | low | blackbox append; panic would hang record_committed and leak append_lock guard |
| `crates/tools/src/eval/idle_timeout.rs:186` | I impl Drop | prod | harmless | low | resumes idle timeout window; in-memory |
| `crates/tools/src/eval/kernel.rs:583` | I impl Drop | prod | harmless | low | schedules cancellation of active cell; in-memory |
| `crates/tools/src/eval/kernel.rs:699` | I impl Drop | prod | harmless | low | seals output sink registry entry; in-memory |
| `crates/tools/src/eval/kernel.rs:1138` | E std thread | prod | cascading | high | Python eval worker thread; any Rust panic in cell handling aborts whole omp |
| `crates/tools/src/eval/kernel.rs:1236` | I impl Drop | prod | harmless | low | marks cell cancelled and schedules interrupt; in-memory |
| `crates/tools/src/eval/kernel.rs:1261` | E std thread | prod | cascading | low | SIGINT monitor thread; trivial loop, panic would abort omp |
| `crates/tools/src/eval/kernel.rs:1385` | I impl Drop | prod | harmless | low | restores SIGINT disposition; only skipped when worker panics, which aborts anyway |
| `crates/tools/src/eval/kernel.rs:1416` | I impl Drop | prod | harmless | low | deregisters SIGINT target; skipped only on worker panic (process aborts) |
| `crates/tools/src/eval/kernel.rs:1437` | I impl Drop | prod | harmless | low | clears worker alive flag; skipped only on worker panic (abort) |
| `crates/tools/src/read.rs:565` | I impl Drop | prod | harmless | low | interrupts sqlite query on future cancellation; cancellation drops still run |
| `crates/tools/src/read.rs:1133` | F spawn_blocking | prod | cascading | high | PDF rasterize of untrusted file; panic hangs tool call, JoinError fallback unreachable |
| `crates/tools/src/read.rs:1513` | F spawn_blocking | prod | cascading | medium | sqlite read; panic hangs read tool instead of 'SQLite read task failed' |
| `crates/tools/src/read.rs:1540` | F spawn_blocking | prod | cascading | high | SVG rasterize untrusted input; panic hangs tool call |
| `crates/tools/src/read.rs:1561` | F spawn_blocking | prod | cascading | high | image processing of untrusted file; panic hangs tool call |
| `crates/tools/src/read/web.rs:152` | F spawn_blocking | prod | cascading | high | image decode of fetched bytes; decoder panic hangs read instead of 'image processing task failed' |
| `crates/tools/src/staging.rs:165` | I impl Drop | prod | harmless | low | removes staged proposal entry; in-memory registry |
| `crates/tools/tests/it/glob.rs:277` | E std thread | test | cascading | low | scoped thread runs glob tool; tool panic aborts test instead of FAILED |
| `crates/tools/tests/it/read.rs:1023` | I impl Drop | test | isolation | low | removes sqlite fixture in shared temp_dir (pid-unique name) |
| `crates/tools/tests/it/shell.rs:636` | E std thread | test | cascading | low | interrupter thread unwrap; panic aborts test process |
| `crates/tui/src/debug.rs:605` | M socket bind | prod | isolation | low | debug socket at env path, pre-removed on start, not removed on exit |
| `crates/tui/src/debug.rs:609` | E std thread | prod | cascading | low | debug server thread; expect on runtime build/serve panic aborts omp (only with OMP_TUI_DEBUG) |
| `crates/tui/src/debug.rs:879` | M socket bind | test | isolation | low | socket in shared temp_dir by pid; removed only at test end |
| `crates/tui/src/graphics.rs:23` | L terminal/termios | prod | harmless | low | termios import |
| `crates/tui/src/graphics.rs:756` | F spawn_blocking | prod | cascading | high | startup probe; panic hangs negotiate_async forever with TTY left raw; fallback unreachable |
| `crates/tui/src/graphics.rs:784` | L terminal/termios | prod | isolation | medium | probe sets real TTY raw with manual restore; panic in probe leaves TTY raw, hook inactive |
| `crates/tui/src/graphics.rs:786` | L terminal/termios | prod | harmless | low | explicit restore on fcntl error |
| `crates/tui/src/graphics.rs:791` | L terminal/termios | prod | harmless | low | explicit restore on fcntl error |
| `crates/tui/src/graphics.rs:797` | L terminal/termios | prod | isolation | medium | normal restore after probe_polled; skipped if parser panics |
| `crates/tui/src/paste.rs:869` | E std thread | prod | cascading | medium | clipboard read thread (native/image decode); panic aborts omp instead of ReadFailure |
| `crates/tui/src/paste.rs:955` | E std thread | prod | cascading | medium | clipboard write thread; panic aborts omp instead of WriteFailure |
| `crates/tui/src/paste.rs:1369` | E std thread | prod | harmless | low | clipboard CLI stdout reader; ok()-based, cannot panic |
| `crates/tui/src/paste.rs:1373` | E std thread | prod | harmless | low | clipboard CLI stderr reader; ok()-based, cannot panic |
| `crates/tui/src/paste.rs:1380` | E std thread | prod | harmless | low | clipboard CLI stdin writer; ok()-based, cannot panic |
| `crates/tui/src/props.rs:1068` | C should_panic | test | harmless | low | should_panic on invalid prop value; pure in-memory |
| `crates/tui/src/pump.rs:161` | E std thread | test | harmless | low | test ingress forwarder thread; trivial loop |
| `crates/tui/src/pump.rs:216` | I impl Drop | prod | harmless | low | aborts actor task, joins bridge thread; in-process |
| `crates/tui/src/pump.rs:354` | E std thread | prod | harmless | low | input bridge thread (non-pollable handles); simple read loop; panic would abort, hook restores tty |
| `crates/tui/src/rich.rs:1120` | I impl Drop | prod | harmless | low | flushes pending rich text into sink; in-memory |
| `crates/tui/src/runtime.rs:1029` | I impl Drop | prod | isolation | low | leaves alt screen, clears layers; hook/Terminal restore covers real TTY |
| `crates/tui/src/runtime.rs:1212` | H multi-thread runtime | test | cascading | low | hold_alt helper child runtime; panicking task aborts helper, parent sees failure |
| `crates/tui/src/runtime.rs:1528` | E std thread | test | harmless | low | trivial UiHandle update thread |
| `crates/tui/src/spelling.rs:156` | E std thread | prod | cascading | medium | native spelling worker; panic aborts whole chat instead of disabling spelling |
| `crates/tui/src/terminal.rs:75` | L terminal/termios | prod | harmless | low | termios import |
| `crates/tui/src/terminal.rs:89` | L terminal/termios | prod | harmless | low | signal/panic-safe termios slot used by emergency restore |
| `crates/tui/src/terminal.rs:94` | L terminal/termios | prod | harmless | low | Sync impl for saved termios slot |
| `crates/tui/src/terminal.rs:96` | L terminal/termios | prod | isolation | low | static termios read by panic hook/signal handler; restore path unaffected by Cranelift |
| `crates/tui/src/terminal.rs:99` | L terminal/termios | prod | harmless | low | original termios state field |
| `crates/tui/src/terminal.rs:263` | I impl Drop | prod | harmless | low | restores fd 2 from saved dup; panic hook emergency_restore_stderr covers it |
| `crates/tui/src/terminal.rs:359` | L terminal/termios | prod | harmless | low | enter raw mode; failure path restores explicitly |
| `crates/tui/src/terminal.rs:360` | L terminal/termios | prod | harmless | low | explicit restore on raw-mode failure; no unwinding involved |
| `crates/tui/src/terminal.rs:373` | L terminal/termios | prod | isolation | low | normal-path restore in leave(); on panic only hook restores |
| `crates/tui/src/terminal.rs:496` | L terminal/termios | prod | isolation | low | emergency tcsetattr in hook/signal path; still effective |
| `crates/tui/src/terminal.rs:708` | I impl Drop | prod | isolation | low | Windows: restores stderr and removes %TEMP% omp-tui-stderr-<pid> file; file leaks |
| `crates/tui/src/terminal.rs:1906` | E std thread | prod | harmless | low | appearance debounce thread; trivial write, no panic sources |
| `crates/tui/src/terminal.rs:2135` | E std thread | prod | harmless | low | progress keepalive thread; trivial writes |
| `crates/tui/src/terminal.rs:2181` | I impl Drop | prod | isolation | medium | leave(): restores termios/alt-screen of real TTY; skipped, relies on panic hook |
| `crates/tui/src/terminal.rs:2380` | B panic hook | prod | isolation | medium | chained hook restores termios/alt-screen/stderr; now the only restore path since Terminal Drop skipped |
| `crates/tui/src/terminal.rs:2381` | B panic hook | prod | isolation | medium | panic hook runs emergency_restore; still works under Cranelift and must keep working |
| `crates/tui/src/terminal.rs:2472` | L terminal/termios | test | harmless | low | test imports for PTY |
| `crates/tui/src/terminal.rs:3081` | L terminal/termios | test | harmless | low | sets raw mode on test PTY, not real TTY |
| `crates/tui/src/terminal.rs:3088` | E std thread | test | harmless | low | raises SIGWINCH in re-executed child test; trivial |
| `crates/tui/src/ui.rs:4477` | C should_panic | test | harmless | low | should_panic on nested overlay assert; pure in-memory |
| `crates/tui/src/watchdog.rs:172` | E std thread | prod | cascading | low | stall watchdog thread; a panicking report callback would abort omp |
| `crates/tui/src/watchdog.rs:226` | I impl Drop | prod | harmless | low | stops and joins watchdog thread |
| `crates/vcs/src/git/cli.rs:150` | J kill_on_drop | prod | isolation | low | Kills git child; orphan may briefly hold .git/index.lock |
| `crates/walker/src/cache.rs:414` | I impl Drop | test | isolation | low | removes uniquely named temp dir; disk only |
| `crates/walker/src/lib.rs:3454` | I impl Drop | prod | harmless | low | macOS: closes directory fd |
| `crates/walker/src/lib.rs:3758` | I impl Drop | prod | harmless | low | Linux: closes fd |
| `crates/walker/src/lib.rs:4054` | I impl Drop | prod | harmless | low | Windows: closes directory handle |
| `crates/walker/src/lib.rs:4303` | I impl Drop | test | isolation | low | removes uniquely named temp tree; disk only |
| `crates/walker/src/lib.rs:4330` | I impl Drop | test | harmless | low | invalidates in-memory walk cache entry |
| `crates/walker/tests/parallel.rs:47` | I impl Drop | test | isolation | low | removes uniquely named temp tree; leak is disk only |
| `crates/webview/src/remote/chromium.rs:407` | J kill_on_drop | prod | isolation | medium | Chromium killed on drop; skipped on panic/abort, browser orphaned with profile |
| `crates/webview/src/remote/firefox.rs:219` | J kill_on_drop | prod | isolation | medium | Firefox killed on drop; skipped on panic/abort |
| `crates/webview/src/remote/mod.rs:160` | I impl Drop | prod | harmless | low | sends Close and joins driver thread; in-process |
| `crates/webview/src/remote/mod.rs:198` | E std thread | prod | cascading | high | browser driver thread; CDP/driver panic aborts omp and orphans Chromium/Firefox child |
| `crates/webview/src/wk/child.rs:169` | I impl Drop | prod | harmless | low | removes script handler and view; in-process |
| `crates/webview/src/wk/frames.rs:791` | I impl Drop | prod | harmless | low | stops capture stream, closes window; in-process macOS resources |
| `crates/webview/src/wk/mod.rs:507` | I impl Drop | prod | harmless | low | unregisters KVO observer; in-process objc state |

## Appendix B: bulk-class sites

Every K (tempfile constructor) and G (tokio spawn) site, by file. Classification rule and exceptions: §3.1.

#### K — tempfile constructors (1379 sites)

- `crates/agent/src/dispatch.rs`: 3401, 3519, 3553
- `crates/agent/src/loop.rs`: 4085, 4131
- `crates/agent/src/pause.rs`: 159
- `crates/agent/src/steering.rs`: 646, 694, 719, 735, 769, 797, 830, 869
- `crates/agent/tests/advisor.rs`: 182, 209, 285, 320, 347
- `crates/agent/tests/approval_desk.rs`: 172
- `crates/agent/tests/approvals.rs`: 43, 62, 141
- `crates/agent/tests/cancel.rs`: 10
- `crates/agent/tests/compaction.rs`: 183, 212, 261, 344, 389, 421, 496, 603, 655
- `crates/agent/tests/directors/harness.rs`: 39
- `crates/agent/tests/dispatch.rs`: 47, 75, 162, 201, 249, 298, 332, 401, 432, 499, 639, 764, 839, 928
- `crates/agent/tests/empty_output.rs`: 77, 143, 208, 236, 275
- `crates/agent/tests/extensions.rs`: 38
- `crates/agent/tests/failure.rs`: 167, 204, 244, 354, 377
- `crates/agent/tests/jobs.rs`: 19, 80, 112, 153, 233, 282, 376, 442
- `crates/agent/tests/lifecycle_hooks.rs`: 309, 389, 473, 547, 611
- `crates/agent/tests/local.rs`: 20, 123
- `crates/agent/tests/output_stream.rs`: 133, 176
- `crates/agent/tests/prompt_golden.rs`: 36
- `crates/agent/tests/runtime_flags.rs`: 32, 62, 97, 129, 195, 234
- `crates/agent/tests/thread_projection.rs`: 75
- `crates/agent/tests/turn.rs`: 133, 203, 270, 316, 358, 397, 431, 524, 625, 674, 742, 788, 815, 849, 901, 1001, 1073, 1173
- `crates/ai/src/auth/aws.rs`: 2175, 2197, 2220, 2240, 2349, 2428, 2456
- `crates/ai/src/auth/key.rs`: 520, 563
- `crates/ai/src/auth/oauth.rs`: 2281, 2315
- `crates/ai/src/auth/store.rs`: 1653, 1677, 1753, 1789, 1812, 1835, 1886, 1946, 1973, 2000, 2039, 2129, 2186, 2230
- `crates/ai/src/discovery/store.rs`: 432, 467, 509, 574
- `crates/ai/src/local/artifact.rs`: 1259, 1294, 1318, 1359
- `crates/ai/src/local/speech_catalog.rs`: 836
- `crates/ai/src/session/mod.rs`: 1797, 1830
- `crates/ai/src/staging.rs`: 641, 644, 1179, 1222, 1287, 1309, 1355, 1409, 1441, 1591, 1594
- `crates/ai/tests/account.rs`: 350
- `crates/ai/tests/local_runtime.rs`: 103
- `crates/ai/tests/support/auth.rs`: 57
- `crates/app/src/acp_events.rs`: 785
- `crates/app/src/chat_cmd.rs`: 1264, 1435, 1463, 1519, 1560, 1634, 1653, 1671, 1697
- `crates/app/src/chat_cmd/launch_input.rs`: 207
- `crates/app/src/chat_control.rs`: 3750, 4121, 4917, 4926
- `crates/app/src/chat_services/agents.rs`: 99, 117
- `crates/app/src/chat_services/control.rs`: 153
- `crates/app/src/chat_services/debug.rs`: 413, 433
- `crates/app/src/chat_services/sessions.rs`: 269
- `crates/app/src/chat_services/stats.rs`: 343
- `crates/app/src/chat_services/workspace.rs`: 160
- `crates/app/src/cli.rs`: 4239, 4495, 4515
- `crates/app/src/cli/bootstrap.rs`: 239, 253
- `crates/app/src/cursor_bridge.rs`: 510
- `crates/app/src/debug.rs`: 93
- `crates/app/src/debug_logs.rs`: 229
- `crates/app/src/diagnostics.rs`: 311
- `crates/app/src/ext_cli/config.rs`: 428
- `crates/app/src/ext_cli/mod.rs`: 3216, 3345
- `crates/app/src/ext_cli/service.rs`: 1108, 1136, 1158, 1183, 1199, 1224, 1259, 1307
- `crates/app/src/gc_cmd.rs`: 426, 453, 520
- `crates/app/src/gui.rs`: 732, 833
- `crates/app/src/print_mode.rs`: 2350, 2393, 2425, 2457, 2503
- `crates/app/src/profile_alias.rs`: 214, 233
- `crates/app/src/render_cmd.rs`: 700, 836
- `crates/app/src/session_import.rs`: 523
- `crates/app/src/startup_notice.rs`: 219
- `crates/app/src/startup_update.rs`: 298, 339
- `crates/app/src/update_cmd.rs`: 1310, 1420, 1446, 1460
- `crates/app/src/welcome_facts.rs`: 156, 216
- `crates/app/src/worktree_cmd.rs`: 564, 580
- `crates/app/tests/acp_spine.rs`: 182, 213, 237, 364, 457
- `crates/app/tests/config.rs`: 25, 26, 30, 55, 56, 59, 82, 83, 86, 111, 112, 115, 185, 186, 189, 221, 222, 225, 257, 263, 284, 287, 305, 306, 312, 334, 337, 349, 352, 364, 367
- `crates/app/tests/env_process_protocol.rs`: 45
- `crates/app/tests/export_cwd.rs`: 9
- `crates/app/tests/it/envd_contract.rs`: 582, 583, 852, 853, 1919, 1920, 1976, 2030, 2233, 2387, 2445, 2556, 2742, 3140, 3185, 3227, 3387
- `crates/app/tests/it/envd_documents.rs`: 73, 216, 278
- `crates/app/tests/it/envd_windows.rs`: 101, 102
- `crates/app/tests/it/envd_workspace.rs`: 57, 58, 135, 136, 165, 166, 167, 168, 215, 216, 331
- `crates/app/tests/it/process_smoke.rs`: 20
- `crates/app/tests/it/stock_sdk_clients.rs`: 69
- `crates/app/tests/it/tool_worker.rs`: 110, 176, 281, 396
- `crates/app/tests/keybindings.rs`: 173, 179
- `crates/app/tests/rpc_spine.rs`: 109, 165, 285, 341, 362, 388, 462, 523
- `crates/app/tests/session_import.rs`: 11, 28, 45, 90, 117
- `crates/app/tests/worktree_query.rs`: 41, 42
- `crates/cache/src/atomic.rs`: 110, 119
- `crates/cache/src/document_cache.rs`: 479, 497
- `crates/cache/src/github_cache.rs`: 335, 400, 432
- `crates/cache/src/mcp_cache.rs`: 231, 262, 286
- `crates/cache/src/secret_key.rs`: 235, 263
- `crates/cache/src/telemetry_cache.rs`: 792, 831, 890
- `crates/chat/src/commands/workspace.rs`: 500
- `crates/chat/src/composer.rs`: 1687, 2014, 2173, 2195, 2221, 2252
- `crates/chat/src/editor.rs`: 315, 342
- `crates/chat/src/gallery.rs`: 125
- `crates/chat/src/gitwatch.rs`: 269, 305, 332
- `crates/chat/src/history.rs`: 414, 437, 503
- `crates/chat/src/media.rs`: 430, 443, 456, 468, 499, 525, 542, 552
- `crates/chat/src/notices/cache.rs`: 262
- `crates/chat/src/notices/divider.rs`: 640
- `crates/chat/src/notices/error.rs`: 471
- `crates/chat/src/notices/retry.rs`: 361, 397
- `crates/chat/src/notify.rs`: 390
- `crates/chat/src/overlays/copy.rs`: 1353, 1434, 1512, 1767, 1882
- `crates/chat/src/overlays/git.rs`: 2775, 3120
- `crates/chat/src/overlays/hub.rs`: 1906, 2209, 2222, 2255
- `crates/chat/src/overlays/move_panel.rs`: 334
- `crates/chat/src/overlays/plan_save.rs`: 327, 343, 366
- `crates/chat/src/overlays/stats.rs`: 495
- `crates/chat/src/project.rs`: 1293, 1477, 1559, 1632, 1707, 1810, 1913, 2044
- `crates/chat/src/status_line.rs`: 390
- `crates/chat/src/transcript.rs`: 857, 1349
- `crates/chat/tests/chrome.rs`: 45
- `crates/chat/tests/host.rs`: 21, 177, 379, 435, 472, 606, 1242, 1806, 1862
- `crates/chat/tests/keys.rs`: 66, 774, 833, 857
- `crates/chat/tests/pi_commands.rs`: 138, 226
- `crates/desktop/src/macos/capture.rs`: 283
- `crates/driver/src/adw/definition.rs`: 269, 314, 346
- `crates/driver/src/cfg.rs`: 480, 505, 526, 543, 570, 586, 603, 614, 642
- `crates/driver/src/collab/observer.rs`: 216
- `crates/driver/src/discovery/active_repo.rs`: 78, 88, 100, 109, 119
- `crates/driver/src/discovery/models.rs`: 1288
- `crates/driver/src/discovery/native.rs`: 656, 678, 705, 730, 750, 771, 799, 834, 879, 903
- `crates/driver/src/discovery/prompts.rs`: 498
- `crates/driver/src/discovery/rules.rs`: 890, 983
- `crates/driver/src/discovery/skills.rs`: 1130, 1163, 1210, 1231, 1252, 1281, 1310, 1353, 1395
- `crates/driver/src/ext_updates.rs`: 546, 597
- `crates/driver/src/headless/con_journal.rs`: 190, 231, 249
- `crates/driver/src/headless/kernel.rs`: 2842, 2947, 3149, 3177, 3203, 3223, 3265, 3324, 3348
- `crates/driver/src/headless/todo.rs`: 166
- `crates/driver/src/prompt_input.rs`: 110
- `crates/driver/src/secrets/config.rs`: 156
- `crates/driver/src/sessions.rs`: 520, 559
- `crates/driver/src/subagent/autoreply.rs`: 492, 563
- `crates/driver/src/subagent/spawn.rs`: 1555, 1576, 1587, 1613, 1655, 1686
- `crates/driver/src/subagent/yield_assembly.rs`: 420, 455
- `crates/driver/src/telemetry_upload.rs`: 204
- `crates/driver/tests/approval_authority.rs`: 124
- `crates/driver/tests/hub_sessions.rs`: 207, 313, 371
- `crates/driver/tests/subagent_cfg.rs`: 9
- `crates/driver/tests/vendor_env_credentials.rs`: 29
- `crates/driver/tests/workpool_producer.rs`: 43
- `crates/driver/tests/workpool_scheduler.rs`: 179
- `crates/e2e/src/baseline.rs`: 102
- `crates/e2e/src/support/owned_groups.rs`: 192
- `crates/e2e/src/support/scratch.rs`: 20
- `crates/e2e/tests/p2_cancel_matrix.rs`: 47, 94
- `crates/e2e/tests/p3_detached_jobs.rs`: 81, 132
- `crates/e2e/tests/p4_schema_isolation.rs`: 108
- `crates/e2e/tests/p5_prefix_stability.rs`: 55, 80
- `crates/e2e/tests/p6_crash_resume.rs`: 483, 582
- `crates/e2e/tests/p7_tui.rs`: 929, 1122
- `crates/e2e/tests/p8_baselines.rs`: 31
- `crates/e2e/tests/p9_extension_control.rs`: 64
- `crates/e2e/tests/p9_isolation.rs`: 81
- `crates/edit/src/path_policy.rs`: 601, 617, 630, 660, 672, 699
- `crates/edit/src/store.rs`: 473
- `crates/edit/tests/common/mod.rs`: 112
- `crates/edit/tests/replace_parity.rs`: 82, 93, 105, 120, 128
- `crates/edit/tests/sloppy.rs`: 36
- `crates/env/src/build_id.rs`: 104
- `crates/envd/src/blobs.rs`: 1215, 1237, 1405, 1432, 1522, 1575, 1598, 1615, 1644
- `crates/envd/src/devices_host.rs`: 693, 716
- `crates/envd/src/docs.rs`: 2311, 2314
- `crates/envd/src/docserver/actor.rs`: 2739, 2759, 2790, 2808, 2843, 2861, 2880, 2897, 2943, 3012, 3073, 3108, 3136, 3183, 3236, 3275, 3326
- `crates/envd/src/docserver/connection.rs`: 686, 797, 814
- `crates/envd/src/docserver/daemon.rs`: 671, 684, 702, 731, 746, 768, 809, 866, 915
- `crates/envd/src/docserver/dap_adapter.rs`: 696, 713, 724, 725, 748
- `crates/envd/src/docserver/environment.rs`: 620, 637, 646, 673, 709
- `crates/envd/src/docserver/fs.rs`: 2909, 2945, 3261
- `crates/envd/src/docserver/lsp_binary.rs`: 142, 162
- `crates/envd/src/docserver/lsp_registry.rs`: 3435, 3558, 4165
- `crates/envd/src/docserver/lsp_supervisor.rs`: 507
- `crates/envd/src/docserver/path_ops.rs`: 783, 813, 858, 913, 963, 1019, 1043, 1073, 1099
- `crates/envd/src/docserver/protocol.rs`: 3309, 3338, 3422
- `crates/envd/src/docserver/transaction.rs`: 2225, 2290, 2347, 2373, 2458, 2570, 2616, 2661, 2707, 2734, 2780, 2816
- `crates/envd/src/docserver/types.rs`: 977, 988
- `crates/envd/src/docserver/watch.rs`: 363
- `crates/envd/src/document_cache.rs`: 108
- `crates/envd/src/eval/process.rs`: 2125, 2426, 2447, 2463, 2538
- `crates/envd/src/eval/tests.rs`: 118
- `crates/envd/src/exec.rs`: 4279, 4439, 4468, 4570, 4614, 4676, 4784, 4804, 4825, 4863, 4892, 4944
- `crates/envd/src/exec_sandbox.rs`: 1237, 1282, 1318, 1338, 1339, 1355, 1380, 1395, 1409, 1410, 1435, 1451, 1463, 1486, 1502, 1503, 1531, 1532, 1555, 1570, 1586, 1604, 1622, 1639, 1657, 1658, 1659, 1681, 1682, 1701
- `crates/envd/src/exthost/dispatch.rs`: 1550
- `crates/envd/src/github.rs`: 2726
- `crates/envd/src/github_url.rs`: 937
- `crates/envd/src/grep.rs`: 1641, 1672, 1688, 1705, 1806
- `crates/envd/src/host_info.rs`: 423, 440
- `crates/envd/src/lib.rs`: 2505, 2539, 2561
- `crates/envd/src/managed_skills.rs`: 362
- `crates/envd/src/mcp/auth_authority.rs`: 350, 385, 472
- `crates/envd/src/mcp/config_store.rs`: 526, 568, 585, 604, 626, 641
- `crates/envd/src/mcp/discovery.rs`: 671, 698, 742
- `crates/envd/src/mcp/manager.rs`: 3844, 3873, 3914, 3949, 4003, 4131
- `crates/envd/src/mcp/mod.rs`: 909, 951, 992
- `crates/envd/src/mcp/smithery.rs`: 1517, 1538, 1554
- `crates/envd/src/mcp/stdio.rs`: 940
- `crates/envd/src/media_devices.rs`: 1509, 1560, 1684, 1725, 1772, 1809, 1810, 1829
- `crates/envd/src/presence.rs`: 295, 296, 313, 314, 326, 327, 354, 355
- `crates/envd/src/process_store.rs`: 454, 487
- `crates/envd/src/report_issue.rs`: 501, 570
- `crates/envd/src/resource_materializer.rs`: 496, 497, 530, 531, 558, 559, 596, 597, 598
- `crates/envd/src/sandbox_proxy.rs`: 68, 70
- `crates/envd/src/server.rs`: 12577, 12578, 12898, 12899, 13031, 13032, 13288, 13489, 13557, 13558, 13597, 13598, 13613, 13699, 13721, 13754, 13755, 13830, 13831, 13861, 13862, 13921, 13922
- `crates/envd/src/site.rs`: 558
- `crates/envd/src/ssh.rs`: 1188
- `crates/envd/src/tool_document.rs`: 2331, 2361, 2463, 2501, 2542, 3477, 3491
- `crates/envd/src/tool_lsp.rs`: 1720, 1739
- `crates/envd/src/tool_read_sources.rs`: 990, 1006, 1028
- `crates/envd/src/tool_read_sources/tests.rs`: 32
- `crates/envd/src/tool_search.rs`: 1595, 1637, 1663, 1686, 1732, 1798, 1837, 1889, 1998, 2026, 2053, 2070, 2148, 2238, 2269, 2302, 2339, 2370, 2431, 2453, 2466
- `crates/envd/src/tool_shell.rs`: 1025, 1042, 1061, 1078, 1193
- `crates/envd/src/tool_url.rs`: 857
- `crates/envd/src/tool_url/local.rs`: 429, 444, 466, 488
- `crates/envd/src/tool_url/vault.rs`: 514
- `crates/envd/src/tools.rs`: 5929
- `crates/envd/src/vault.rs`: 1459, 1508, 1544, 1559, 1613, 1688, 1742, 1768
- `crates/envd/src/vcs/git/tests.rs`: 35, 142, 158, 173
- `crates/envd/src/worker.rs`: 4273
- `crates/envd/src/workspace.rs`: 490, 514, 571, 588, 608, 609
- `crates/envd/src/workspace/operations.rs`: 1911
- `crates/envd/tests/credential_secret_authority.rs`: 22
- `crates/envd/tests/docserver_authority_client.rs`: 89
- `crates/envd/tests/docserver_lsp_supervisor.rs`: 84, 131
- `crates/envd/tests/document_config_root.rs`: 10, 11, 12
- `crates/envd/tests/durable_schedules.rs`: 126, 145, 176, 200, 257, 300
- `crates/envd/tests/host_owner_backends.rs`: 211
- `crates/envd/tests/production_control_factory.rs`: 27, 28
- `crates/ext/src/doctor.rs`: 393
- `crates/ext/src/lock.rs`: 712
- `crates/ext/src/trust.rs`: 774, 863
- `crates/ext/src/upgrade.rs`: 967, 1067
- `crates/journal/src/blob.rs`: 1216, 1230, 1241, 1252, 1262, 1284
- `crates/journal/tests/gc.rs`: 48, 99, 138, 156, 185, 284, 318, 366, 413, 431, 481, 528, 572, 590
- `crates/journal/tests/journal.rs`: 45, 81, 112, 153, 170, 185
- `crates/rpc/src/hello.rs`: 174, 191, 209
- `crates/sandbox/src/backends/bubblewrap.rs`: 473, 474
- `crates/sandbox/src/backends/docker.rs`: 766
- `crates/sandbox/src/backends/landlock.rs`: 1017
- `crates/sandbox/src/backends/seatbelt.rs`: 627, 651, 652
- `crates/sandbox/src/runner.rs`: 740
- `crates/sandbox/src/runtime/bubblewrap.rs`: 27, 40, 42
- `crates/sandbox/src/runtime/gvisor.rs`: 152
- `crates/sandbox/src/runtime/linux_view.rs`: 56, 75, 280, 296, 315
- `crates/sandbox/src/runtime/macos.rs`: 35, 37
- `crates/sandbox/src/runtime/windows.rs`: 73, 75, 1199, 1201
- `crates/sandbox/tests/appcontainer.rs`: 18, 60, 81, 107, 127, 169, 170, 230
- `crates/sandbox/tests/bubblewrap.rs`: 81, 113, 132, 157, 172, 201, 250, 273, 325, 403, 456, 487, 548
- `crates/sandbox/tests/core.rs`: 126, 290
- `crates/sandbox/tests/docker.rs`: 90, 222, 252, 296, 362, 389
- `crates/sandbox/tests/gvisor.rs`: 145, 166, 183, 204, 224, 344, 378
- `crates/sandbox/tests/landlock.rs`: 51, 68
- `crates/sandbox/tests/seatbelt.rs`: 93, 142, 162, 182, 204, 388, 409, 431, 449, 450, 469, 513, 554, 591, 612, 623, 651
- `crates/sdk/src/lib.rs`: 142
- `crates/secrets/tests/key_persistence.rs`: 11
- `crates/session/src/exit_diagnostics.rs`: 641
- `crates/session/tests/appendix_a.rs`: 49, 68, 84, 97, 114, 133, 151, 162, 190
- `crates/session/tests/projection.rs`: 103, 171, 225, 313, 427, 475, 573, 607, 647, 739, 791, 854, 947, 1013, 1085, 1146, 1203, 1290, 1347
- `crates/session/tests/replay_law.rs`: 87, 160, 184, 217, 242, 260, 301, 356, 412, 481, 633, 674
- `crates/session/tests/rewind.rs`: 24, 62, 124, 217
- `crates/shell-builtins/src/base32.rs`: 540
- `crates/shell-builtins/src/base64.rs`: 105
- `crates/shell-builtins/src/cat.rs`: 688
- `crates/shell-builtins/src/cksum.rs`: 2155, 2233, 2250
- `crates/shell-builtins/src/cmp.rs`: 581, 582, 589, 600, 612, 624, 642
- `crates/shell-builtins/src/combine.rs`: 279, 325, 334, 343, 362, 373
- `crates/shell-builtins/src/comm.rs`: 433, 443, 459, 470, 480
- `crates/shell-builtins/src/cut.rs`: 1057
- `crates/shell-builtins/src/diff.rs`: 928, 937, 948, 960, 971, 990, 1006, 1018, 1042, 1052, 1070, 1086, 1100, 1112, 1126, 1139, 1154, 1164, 1173, 1184, 1194, 1203, 1213, 1227, 1243, 1269, 1291, 1302
- `crates/shell-builtins/src/dyn.rs`: 1373, 1541
- `crates/shell-builtins/src/fd.rs`: 1576
- `crates/shell-builtins/src/find.rs`: 5166, 5181, 5287
- `crates/shell-builtins/src/grep.rs`: 1698, 1723, 1741, 1751
- `crates/shell-builtins/src/head.rs`: 1557, 1568, 1587, 1596
- `crates/shell-builtins/src/host.rs`: 1575, 1628
- `crates/shell-builtins/src/isutf8.rs`: 270, 278, 294, 304, 318, 328, 336, 349, 359, 368
- `crates/shell-builtins/src/jq.rs`: 1141, 1473, 1484, 1494, 1552, 1585
- `crates/shell-builtins/src/ln.rs`: 635, 636, 644, 666, 675, 693, 708, 720, 730, 738, 751, 762, 771
- `crates/shell-builtins/src/ls.rs`: 5482, 5492, 5508, 5526, 5537, 5551
- `crates/shell-builtins/src/md5sum.rs`: 57, 67, 78, 90, 101
- `crates/shell-builtins/src/mkdir.rs`: 388, 396, 404, 412, 425
- `crates/shell-builtins/src/mktemp.rs`: 478, 565, 566, 608, 627, 638, 650, 662, 674, 683, 697, 708, 722, 731, 742, 755, 764, 777, 786
- `crates/shell-builtins/src/mv.rs`: 1958, 1971, 1985, 1998, 2015
- `crates/shell-builtins/src/paste.rs`: 407, 419, 461
- `crates/shell-builtins/src/readlink.rs`: 236, 237, 245, 260, 275, 286, 297, 312, 328
- `crates/shell-builtins/src/realpath.rs`: 365, 366, 375, 387, 397, 409, 420, 432, 444
- `crates/shell-builtins/src/rg.rs`: 1969, 2021, 2034, 2044, 2067, 2082, 2095, 2110, 2151
- `crates/shell-builtins/src/rm.rs`: 1721, 1734, 1753, 1765, 1784
- `crates/shell-builtins/src/sed.rs`: 2862, 6014, 6040, 6049, 6077, 6084, 6125, 6136, 6163, 6175, 6201, 6210, 6236, 6245, 6271, 6321, 7554, 7688, 7705, 7727, 7751, 9082, 9100, 9123, 9126, 9731, 9741
- `crates/shell-builtins/src/sort.rs`: 1278, 2445, 2447, 5766
- `crates/shell-builtins/src/sponge.rs`: 213, 234, 242, 251, 258, 276, 288, 297
- `crates/shell-builtins/src/stat.rs`: 2669, 2670, 2677, 2690, 2703, 2716, 2726, 2739, 2753, 2785, 2816, 2828, 2842, 2856, 2873, 2896, 2936, 2954, 2964, 2978, 2989, 3014, 3025, 3036, 3047, 3059, 3069
- `crates/shell-builtins/src/support/backup.rs`: 369
- `crates/shell-builtins/src/support/fsutil.rs`: 447, 487
- `crates/shell-builtins/src/support/safe_traversal.rs`: 198
- `crates/shell-builtins/src/tac.rs`: 437, 438, 445, 497
- `crates/shell-builtins/src/tail.rs`: 3678, 3688, 3725, 3736, 3755, 3766
- `crates/shell-builtins/src/tee.rs`: 394, 403, 412, 421, 451
- `crates/shell-builtins/src/timeout.rs`: 483, 641
- `crates/shell-builtins/src/touch.rs`: 1089, 1090, 1111, 1126, 1141, 1168, 1228, 1272, 1299, 1335
- `crates/shell-builtins/src/truncate.rs`: 405, 406, 417, 427, 437, 449, 458, 467, 478, 489, 500, 526, 536, 551, 564, 577
- `crates/shell-builtins/src/uniq.rs`: 1060, 1071
- `crates/shell-builtins/src/wc.rs`: 1623, 1634, 1645, 1656, 1666
- `crates/shell-builtins/src/which.rs`: 115, 131, 145, 158, 180, 195
- `crates/shell-builtins/src/xargs.rs`: 1450, 1511, 1532, 1541
- `crates/shell-builtins/tests/dyn.rs`: 163, 182, 204, 217, 251, 265
- `crates/shell-builtins/tests/exec.rs`: 14, 345
- `crates/shell/src/completion.rs`: 1636
- `crates/shell/src/interp.rs`: 2613, 2614, 2639, 2640, 2659, 2688
- `crates/shell/src/patterns.rs`: 896, 932, 971
- `crates/shell/src/sys/unix/poll.rs`: 184
- `crates/tools/src/ast_edit.rs`: 1010, 1067, 1082, 1116, 1188, 1233, 1257
- `crates/tools/src/ast_grep.rs`: 1020, 1040, 1052, 1091, 1163
- `crates/tools/src/edit/observer.rs`: 668
- `crates/tools/src/eval/kernel.rs`: 2399, 2400
- `crates/tools/src/read/sqlite.rs`: 1259
- `crates/tui/src/paste.rs`: 85
- `crates/tui/src/theme.rs`: 777
- `crates/vcs/src/git/diff.rs`: 1350
- `crates/vcs/src/git/mod.rs`: 423, 437, 453
- `crates/vcs/src/git/mutate.rs`: 1381
- `crates/vcs/src/git/patch.rs`: 2781, 3291, 3333, 3374, 3432, 3483, 3525, 3571, 3613, 3657, 3691, 3743, 3767, 3821, 3941, 4016, 4076, 4104, 4235, 4309, 4419, 4441, 4497, 4674, 4692, 4760, 4935, 4960, 5049, 5050, 5083, 5084, 5126, 5772, 5818
- `crates/vcs/src/git/read.rs`: 1193
- `crates/vcs/src/jj/ops.rs`: 1093, 1135, 1176
- `crates/vcs/src/lib.rs`: 323, 329, 338, 363, 384
- `crates/webview/src/remote/mod.rs`: 267

#### G — tokio task spawns (400 sites)

- `crates/agent/src/dispatch.rs`: 1404, 1970
- `crates/agent/src/file_mentions.rs`: 75
- `crates/agent/tests/advisor.rs`: 358
- `crates/agent/tests/approval_desk.rs`: 197
- `crates/agent/tests/approvals.rs`: 90, 111, 129
- `crates/agent/tests/cancel.rs`: 22, 48, 49, 63, 64, 65
- `crates/agent/tests/compaction.rs`: 460
- `crates/agent/tests/jobs.rs`: 47, 198, 309, 405
- `crates/agent/tests/lifecycle_hooks.rs`: 200, 285, 372, 406, 460, 490, 534, 565, 603
- `crates/agent/tests/thread_projection.rs`: 56
- `crates/agent/tests/turn.rs`: 752, 938, 952, 1011
- `crates/ai/src/auth/alibaba_token_plan.rs`: 315, 722, 826
- `crates/ai/src/auth/manager.rs`: 545, 688, 840, 1723
- `crates/ai/src/auth/oauth/callback.rs`: 85
- `crates/ai/src/auth/oauth/custom/cursor.rs`: 642
- `crates/ai/src/local/artifact.rs`: 1333, 1345
- `crates/ai/src/realtime/live.rs`: 445, 446, 696
- `crates/ai/src/transport/cassette.rs`: 835
- `crates/ai/src/transport/tests.rs`: 233, 1163, 1205, 1255, 1339, 1401, 1439, 1466
- `crates/ai/src/transport/websocket_transport.rs`: 351, 751
- `crates/app/src/acp_mode.rs`: 170, 505, 663, 751, 960
- `crates/app/src/chat_cmd.rs`: 1184, 1200
- `crates/app/src/chat_control.rs`: 440, 508, 1943, 2152, 2949, 3287, 3816, 4178, 4637
- `crates/app/src/chat_services/mcp.rs`: 151, 214, 245, 301
- `crates/app/src/chat_services/session_ops.rs`: 294
- `crates/app/src/chat_voice.rs`: 564, 722, 2891
- `crates/app/src/daemon.rs`: 351, 376, 395
- `crates/app/src/live_reachability.rs`: 15, 235
- `crates/app/src/rpc_mode.rs`: 1272, 1333, 1388
- `crates/app/src/startup_update.rs`: 87, 270
- `crates/app/tests/it/envd_contract.rs`: 620, 631, 666, 1863, 3282
- `crates/app/tests/it/envd_documents.rs`: 84
- `crates/app/tests/it/envd_windows.rs`: 50, 121
- `crates/app/tests/it/envd_workspace.rs`: 38, 43
- `crates/app/tests/rpc_spine.rs`: 531
- `crates/app/tests/worktree_query.rs`: 25
- `crates/chat/src/host.rs`: 999
- `crates/chat/src/notices/voice.rs`: 546
- `crates/driver/src/cleanse/continuous.rs`: 604
- `crates/driver/src/collab/session.rs`: 737, 828, 927, 1160, 1309, 2019
- `crates/driver/src/ext_updates.rs`: 183
- `crates/driver/src/headless/ask.rs`: 173, 250
- `crates/driver/src/headless/kernel.rs`: 2845
- `crates/driver/src/registry.rs`: 1181
- `crates/driver/src/subagent/autoreply.rs`: 223
- `crates/driver/src/subagent/hub.rs`: 864
- `crates/driver/src/subagent/revive.rs`: 155
- `crates/driver/src/subagent/spawn.rs`: 829, 1271, 1437
- `crates/driver/src/subagent/workpool_runtime.rs`: 221
- `crates/driver/src/subagent/workpool_scheduler.rs`: 650
- `crates/driver/tests/approval_authority.rs`: 166
- `crates/driver/tests/workpool_producer.rs`: 184
- `crates/driver/tests/workpool_scheduler.rs`: 89, 209
- `crates/e2e/src/support/docserver.rs`: 40
- `crates/e2e/src/support/envd.rs`: 93, 307
- `crates/e2e/tests/p9_isolation.rs`: 186, 194
- `crates/env/src/client.rs`: 2357, 4562, 4603, 4628, 4649, 4749
- `crates/env/src/partition.rs`: 103, 604, 606, 652
- `crates/env/src/windows.rs`: 132
- `crates/env/tests/extension_client.rs`: 46, 103
- `crates/envd/src/admission.rs`: 1021, 1059
- `crates/envd/src/browser_relay.rs`: 544, 586, 652, 889, 1431, 1441, 1989, 2172, 2180, 2632, 2720, 3246, 3384, 3716, 3737
- `crates/envd/src/docs.rs`: 1700, 1724, 1795, 2227, 2341, 2362
- `crates/envd/src/docserver/actor.rs`: 1160, 3150, 3162, 3254, 3287, 3307, 3340
- `crates/envd/src/docserver/connection.rs`: 33, 195, 227, 237, 284, 511, 699, 701, 825, 885
- `crates/envd/src/docserver/daemon.rs`: 398, 480, 502, 778, 817, 874
- `crates/envd/src/docserver/dap_protocol.rs`: 152, 697
- `crates/envd/src/docserver/dap_session.rs`: 454, 482, 530, 1560, 1613
- `crates/envd/src/docserver/lsp.rs`: 1984, 2017, 2075
- `crates/envd/src/docserver/lsp_pool.rs`: 194
- `crates/envd/src/docserver/lsp_process.rs`: 686, 695, 1427, 1625, 1638, 1660, 1695, 1745, 1751
- `crates/envd/src/docserver/lsp_registry.rs`: 27, 710, 822, 1259, 1873, 3682, 3690, 3726, 3848, 4192
- `crates/envd/src/docserver/lsp_supervisor.rs`: 189, 299
- `crates/envd/src/docserver/path_ops.rs`: 821, 925
- `crates/envd/src/docserver/protocol.rs`: 1265
- `crates/envd/src/docserver/transaction.rs`: 907, 2591
- `crates/envd/src/docserver/windows.rs`: 54
- `crates/envd/src/document_cache.rs`: 82
- `crates/envd/src/eval/process.rs`: 43, 336, 504, 733, 1548, 1610
- `crates/envd/src/exec.rs`: 772, 1196, 1630, 1791, 3082, 3217
- `crates/envd/src/exthost/control.rs`: 2860
- `crates/envd/src/exthost/spawn.rs`: 183, 688
- `crates/envd/src/lib.rs`: 142, 890, 977, 996, 1004, 1020, 1026, 1100, 1497, 1515, 1602, 2060, 2106, 2133, 2170, 2262
- `crates/envd/src/mcp/http.rs`: 904, 1303
- `crates/envd/src/mcp/manager.rs`: 1083, 1087, 1211, 1227, 1232, 1278, 1380, 1385, 2135, 2297, 2438, 2731, 4037
- `crates/envd/src/mcp/stdio.rs`: 533, 534
- `crates/envd/src/resource_materializer.rs`: 129
- `crates/envd/src/schedules.rs`: 384
- `crates/envd/src/security_scan.rs`: 278
- `crates/envd/src/server.rs`: 77, 860, 2432, 3153, 3718, 6116, 8205, 9092, 9488, 9855, 9902, 10130, 10182, 10269, 10412, 10467, 10704, 11593, 11819, 11838, 12077, 12200, 12466, 12514, 12588, 12706, 12737, 12929, 13075, 13077, 13617, 13639, 13678, 13766, 13800, 13837, 13890, 13956, 13965
- `crates/envd/src/ssh.rs`: 29, 726, 757, 882
- `crates/envd/src/tool_document.rs`: 1931, 2554, 3353, 3369, 3398, 3450
- `crates/envd/src/tool_read_sources/tests.rs`: 76, 82, 328, 365, 420
- `crates/envd/src/tool_search.rs`: 1548, 2412
- `crates/envd/src/tool_shell.rs`: 1097, 1197, 1202
- `crates/envd/src/tools.rs`: 1478, 1483, 1801, 2832, 2914
- `crates/envd/src/vault.rs`: 1709
- `crates/envd/src/vcs/git/refs.rs`: 121
- `crates/envd/src/windows.rs`: 16, 78, 189, 196, 199
- `crates/envd/src/worker.rs`: 1911, 1940, 2937, 3784, 3788
- `crates/envd/tests/docserver_authority_client.rs`: 98
- `crates/envd/tests/policy_placement_params_authority.rs`: 423
- `crates/oauth/src/http.rs`: 238
- `crates/rpc/src/client.rs`: 523, 545, 548, 639, 763, 784, 826
- `crates/rpc/src/hello.rs`: 158
- `crates/rpc/src/uds.rs`: 119
- `crates/sandbox/src/runner.rs`: 914, 925, 994
- `crates/sandbox/src/runtime/gvisor.rs`: 588, 662
- `crates/serve/src/auth.rs`: 209
- `crates/serve/src/inference.rs`: 864, 1519
- `crates/shell/src/commands.rs`: 582, 982
- `crates/shell/src/interp.rs`: 735, 1093, 1260, 2485
- `crates/shell/src/processes.rs`: 214
- `crates/shell/src/shell.rs`: 736
- `crates/tools/src/edit/observer.rs`: 753, 783, 803
- `crates/tools/src/eval/idle_timeout.rs`: 341
- `crates/tools/tests/it/shell.rs`: 742, 755
- `crates/tui/examples/tml.rs`: 56
- `crates/tui/src/debug.rs`: 885
- `crates/tui/src/pump.rs`: 312
- `crates/tui/src/runtime.rs`: 897
- `crates/vcs/src/git/cli.rs`: 166, 167

## Appendix C: probe sources

These are the temporary probes behind the [M] claims. None of them is committed. The in-repo probes were deleted and every mutation was reverted before this report was committed.

### C.1 Minimal workspace probe (§1)

`.cargo/config.toml`:

```toml
[unstable]
codegen-backend = true
[profile.dev]
debug = false
codegen-backend = "cranelift"
[profile.dev.package."*"]
codegen-backend = "llvm"
[profile.dev.package.llvmlib]
codegen-backend = "llvm"
[profile.dev.build-override]
codegen-backend = "llvm"
```

Root `Cargo.toml`: members `clif` and `llvmlib`, `[profile.dev] incremental = false, codegen-units = 256`, and a copy of the repo's `rust-toolchain.toml`. `clif` depends on `llvmlib` and on `tokio = { version = "1", features = ["rt", "rt-multi-thread", "macros"] }`.

`llvmlib/src/lib.rs`:

```rust
use std::panic::{self, AssertUnwindSafe};
pub struct Guard(pub &'static str);
impl Drop for Guard { fn drop(&mut self) { eprintln!("DROP {}", self.0); } }
pub fn catch_dyn(f: &dyn Fn()) -> bool { panic::catch_unwind(AssertUnwindSafe(|| f())).is_err() }
pub fn catch_generic<F: FnOnce()>(f: F) -> bool { panic::catch_unwind(AssertUnwindSafe(f)).is_err() }
pub fn guard_llvm_frame(f: &dyn Fn()) { let _g = Guard("llvm-frame"); f(); }
pub fn preinstantiate() -> bool { catch_generic::<fn()>(noop as fn()) }
fn noop() {}
pub fn spawn_dyn(f: Box<dyn FnOnce() + Send>) -> bool { std::thread::spawn(f).join().is_err() }
```

`clif/src/main.rs`: one arm per row of the §1 table. Each arm is run as `./clif <scenario>` under `timeout 10` with `RUST_BACKTRACE=0`.

```rust
"catch_unwind_clif" => panic::catch_unwind(|| { let _g = Guard("clif-inner"); boom() }).is_err(),
"thread_spawn_clif" => std::thread::spawn(|| { let _g = Guard("clif-thread"); boom() }).join().is_err(),
"catch_dyn_llvm" => llvmlib::catch_dyn(&|| { let _g = Guard("clif-closure"); boom() }),
"catch_generic_llvm_closure" => llvmlib::catch_generic(|| { let _g = Guard("clif-closure"); boom() }),
"catch_generic_llvm_fnptr" => llvmlib::catch_generic::<fn()>(boom_fnptr as fn()),
"tokio_spawn" => rt_multi.block_on(async { tokio::spawn(async { let _g = Guard("tokio-task"); boom() }).await }),
"tokio_spawn_blocking" => rt_multi.block_on(async { tokio::task::spawn_blocking(|| boom()).await }),
"tokio_current_thread" => rt_current.block_on(async { tokio::spawn(async { boom() }).await }),
// … plus guard_in_llvm_frame, guard_in_clif_frame_under_llvm_catch, spawn_dyn_llvm, panic_hook_runs
```

`clif/tests/libtest.rs`:
- a `#[test]` holding a guard whose `Drop` writes a marker file, then panics;
- a `#[should_panic(expected = "boom")]` test.

### C.2 In-repo probes (temporary; deleted)

| File | Probe | Evidence for |
|---|---|---|
| `crates/shell-builtins/src/host.rs` (appended `#[cfg(test)] mod zz_cranelift_probe`) | A `Boom` `Utility` that panics. `run_caught(Boom, &mut Host::for_test("boom", "", "/"))` must return 1. The same call inside `tokio::task::spawn_blocking` must resolve within 10 s. | §3.3 item 1 |
| `crates/session/tests/zz_observability_probe.rs` | `TelemetryConfig` with a panicking `normalize_provider`, a panicking `cost_estimator` reached through `estimate_cost(&ctx, \|_\| Some(…))`, and a warning hook that locks a `std::sync::Mutex` and panics; afterwards `try_lock` classifies the mutex as unlocked, poisoned or still locked. | §4 |
| `crates/py/tests/zz_pyo3_panic_probe.rs` | `#[pyfunction] fn boom() -> PyResult<u32> { panic!(…) }`, called from Python as `try: boom() except BaseException as error: caught = type(error).__name__`; asserts `PanicException`. | §3.3 item 2 |
| `crates/envd/tests/zz_leak_probe.rs` | `tokio::process::Command::new("/bin/sleep").arg("3131").kill_on_drop(true)` with null stdio, then `panic!`; afterwards `pgrep -x sleep`. A second test used `ExecHost::start_process` (the negative result in §3.3). | §3.3 items 7–8 |
| `crates/e2e/tests/zz_worker_panic_probe.rs` | A multi-thread `#[tokio::test]` leases `OwnedProcess::spawn(sleep 3232)` on the libtest thread, then awaits `tokio::spawn(async { panic!() })`. | §3.3 item 3 |
| Mutation: `crates/e2e/src/support/owned_groups.rs` | `HOOK.call_once(install_hook);` replaced by a no-op. | §6.1 |
| Mutation: `crates/core/src/append_vec.rs:1346` | `assert_eq!(vec.len(), 2)` changed to `999`. | §3.3 item 4 |

## Appendix D: reproduction commands

Run every command from the repo root at the audited commit, after `just setup-python`. `T` is a scratch target dir. `LLVM_TEST` stands for `--config 'profile.test.codegen-backend="llvm"'`.

```bash
# §3–§4 probes (after adding the Appendix C.2 files)
CARGO_TARGET_DIR=$T cargo nextest run --locked -p omp-shell-builtins -p omp-session -p omp-envd \
  -E 'test(/zz_/)' --no-fail-fast --failure-output final --success-output final
CARGO_TARGET_DIR=$T cargo nextest run --locked $LLVM_TEST -p omp-shell-builtins -p omp-session -p omp-envd \
  -E 'test(/zz_/)' --no-fail-fast --failure-output final --success-output final
cargo nextest run --locked --config 'profile.dev.package.omp-observability.codegen-backend="cranelift"' \
  -p omp-session --test zz_observability_probe          # the override removed
cargo nextest run --locked [$LLVM_TEST] -p omp-py --test zz_pyo3_panic_probe --success-output final

# §6.2 per-crate SIGSEGV (5 of 5), then the homogeneous control (16 of 16 pass)
cargo nextest run --locked --config 'profile.dev.package.omp-e2e.codegen-backend="llvm"' \
  -p omp-e2e --test p6_crash_resume -E 'test(p6_resume_preserves)'
TERM=xterm-256color cargo nextest run --locked $LLVM_TEST -p omp-e2e --lib --test p1_doc_race \
  --test p2_cancel_matrix --test p3_detached_jobs --test p4_schema_isolation \
  --test p5_prefix_stability --test p6_crash_resume --test p7_tui --no-fail-fast

# §6.2 whole workspace under the LLVM test profile
cargo nextest run --workspace --exclude omp-e2e --locked $LLVM_TEST -E 'not test(/zz_/)' --no-fail-fast

# §6.4 cold builds (fresh target dir each time)
rm -rf $T && CARGO_TARGET_DIR=$T cargo build -p omp-app --bin omp --locked [--config 'profile.dev.codegen-backend="llvm"']
rm -rf $T && CARGO_TARGET_DIR=$T cargo nextest run -p omp-e2e --tests --no-run --locked [$LLVM_TEST]
```

In the `omp-py` line, run once without `$LLVM_TEST` and once with it. The optional `--config` in the cold-build lines selects the all-LLVM variant.
