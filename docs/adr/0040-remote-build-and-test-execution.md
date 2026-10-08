# 0040. Build and test commands are placed by an omp-owned execution router; BuildBuddy is one replaceable REAPI backend

Status: proposed
Date: 2026-10-08
Area: runtime

## Context

The owner's development machine is a 16 GB Apple-silicon Mac, and it swaps under serialized cargo
builds of this workspace. On 2026-10-08 the owner asked for part of the compilation and
test/verification work to move to BuildBuddy Remote Execution or Remote Cache **soon**, with three
conditions:

- **No Bazel migration.**
- **BuildBuddy is never a hard dependency.** OMP² and the future `omp adw` products (Rust/Cargo,
  Maven/Gradle, Kotlin, Go, .NET, npm/pnpm, Python, Bazel) own the abstraction: a build-system
  adapter produces an execution plan, a router places it, and a backend runs it. Backends are
  local, BuildBuddy, and later SSH, Kubernetes or any other REAPI server.
- **Practical first.** The owner's clarification overrides a framework-first reading. Every option
  is ranked by how much local CPU, RAM, disk and wall time it removes in the near term, and at what
  adoption friction.

Citation rules for this record:

- Repository claims cite `path:line` at `omp2` `929f902235`.
- External claims cite the pinned sources in the next table.
- **[I]** marks an inference.
- **UNVERIFIED** marks what could not be checked without a BuildBuddy account or a live run.

### Source pins

| Key | Source |
|---|---|
| BB | `https://github.com/buildbuddy-io/buildbuddy/blob/7f12d987923b291095dcee0f2674b2dc06d1bd4d/` (HEAD on 2026-10-08). The public docs at `https://www.buildbuddy.io/docs/<id>/` are built from `docs/<id>.md` in this tree. |
| REAPI | `https://github.com/bazelbuild/remote-apis/blob/6def1c5d27a527c400875c24ae8b1a160145d7e1/build/bazel/remote/execution/v2/remote_execution.proto` (`README.md` in the same tree is "REAPI README") |
| LRO | `https://github.com/googleapis/googleapis/blob/6553725b30f5bdad83dbc42c8c7ee3cada14b82e/google/longrunning/operations.proto` |
| SCC | `https://github.com/mozilla/sccache/blob/214cde1c19b4c5962283d22b67801ce53ed17d51/` |
| BR | `https://github.com/buchgr/bazel-remote/blob/47e1d1a8d60899354524fa0c9e8115dfed43fcb9/README.md` |
| Cargo | `https://doc.rust-lang.org/cargo/reference/config.html#env` and `https://doc.rust-lang.org/cargo/reference/unstable.html#checksum-freshness` |

### Problem

Three facts make this harder than pointing cargo at a remote cache.

1. **Cargo is not a REAPI client, and nothing in omp speaks REAPI.** No crate references REAPI,
   BuildBuddy or Bazel's remote APIs. The only `bazel` hits are a Starlark language alias
   (`crates/ast/src/language/mod.rs:487`) and memory-import test text. The repo tracks no
   `.bazelrc`, `MODULE.bazel` or `BUILD` file.
2. **The heavy local work is the test and lint half, and much of it is link-bound.** Test builds
   compile every member a second time under LLVM (`.cargo/config.toml:96-106`). Clippy keeps its
   own target dir (`justfile:61-67`). Each test binary statically links the ~650 MB CPython
   (`.github/workflows/ci.yml:244-249`). A compiler cache never caches links, binaries or
   proc-macros (SCC `docs/Rust.md:1-14`).
3. **This laptop cannot produce Linux results, and Linux executors cannot produce its macOS
   binary.**
   - BuildBuddy Cloud's managed executors are Linux; `darwin` and `windows` are self-hosted only
     (BB `docs/rbe-platforms.md:239`).
   - The macOS link uses Homebrew `ld64.lld` at an absolute path (`.cargo/config.toml:28-35`).
   - 354 `target_os = "macos"` sites (envd 65, shell-builtins 60, webview 59, sandbox 39, ai 29)
     are exercised only by a macOS run.

### Where the Mac's build load comes from (evidence)

**Repository evidence.** All measured on this host model (M1 Pro, 8P+2E cores, 16 GB) unless a
row says otherwise.

| Work | Measurement | Source |
|---|---|---|
| Cold `cargo build -p omp-app --bin omp` | 309-731 s (Cranelift mean 535 s), 944 units | `docs/audits/cranelift-panic-cleanup-mac-followup.md:45-66` |
| One inner-loop turn: edit `omp-core`, rebuild `omp`, rebuild the e2e tests | 352.9 s, of which the test build is **165.4 s (47%)** | same file, `:45-54` |
| Why the test half is heavy | It is dominated by linking the `omp-e2e` test binaries, each with the embedded CPython; memory pressure is a candidate cause of the noise [I] | same file, `:72`, `:76-80` |
| First test build after a dev build | 238.2 s. Test builds recompile the members (51 units against 17). | same file, `:50`, `:70` |
| Warm sccache with a fresh target dir | 131-143 s against 261-577 s cold | `docs/audits/sccache-mac-measurement.md:60-90` |
| Second worktree with a shared sccache | Only 70% hits, likely because `CARGO_MANIFEST_DIR`/`OUT_DIR` absolute paths enter the key [I]; new content is about 35% slower | same file, `:90-95` |
| Linux reference host (4 vCPU, 15 GiB) | Cold `omp` 405.8 s / 5.1 GB. Cold e2e test build 427.6 s / 7.3 GB. Cold workspace test build 656 s / 18 GB. | `docs/audits/cranelift-panic-cleanup.md:8`, `:486-508` |

**Observed on the owner's machine (2026-10-08).** This is not repository evidence.

- **The machine.** `sysctl` reports `MacBookPro18,3`, 16 GB, 10 CPUs. That is the self-hosted CI
  runner model named in both audits.
- **CI runs on it.**
  - A GitHub Actions runner service for `blyzer/oh-my-pi` is installed and listening.
  - Its worker logs show 52 `Rust workspace and acceptance proofs` jobs between 2026-10-03 and
    2026-10-05: median 14.2 min, p90 20.7 min, 713 min in total. On 2026-10-05 alone, CI used
    530 min of the machine.
  - The same logs show 25 `Record P8 baseline` jobs (median 2.5 min) and one packaging job.
  - The latest omp job ended 2026-10-05 17:49 UTC. Whether `vars.MACOS_RUNNER` still points here
    is UNVERIFIED (`gh` did not answer).
  - Two more runner services, for unrelated repositories, ran jobs on 2026-10-08.
- **Disk.**
  - `~/Library/Caches/omp-ci` holds 96 GB.
  - The main checkout's `target/` holds `debug` 28 GB, `release` 8.2 GB and `clippy` 2.6 GB.
  - Four `omp2-wt-*` worktrees hold 17-22 GB of `target/` each.
  - 36 GiB of disk was free.
- **Memory.** 13.7 of 15.4 GB of swap was in use while this record was written.
- **No RAM measurement.** The repository has no peak-RSS measurement of any recipe.

**Reading [I].** Two sources load the laptop:

1. **CI jobs** (the macOS `rust` job: clippy, workspace nextest, doctests and e2e; `ci.yml:230-375`).
2. **Repeated test-profile and clippy builds in several worktrees.**

The second source can move to a Linux executor for its Linux-valid part. The first cannot move to
BuildBuddy Cloud as a whole, because it is a macOS job. Moving it to a hosted macOS runner or
another Mac is an owner decision outside this ADR, listed as R0 below. Note that the Linux
workspace test suite is gated nowhere:
- `ci.yml` runs workspace nextest only in the macOS job (`:338`).
- Its Linux jobs run clippy and e2e subsets (`:440`, `:515-522`, `:588`).
- `AGENTS.md:86` claims otherwise.

### What exists in the tree to build on

| Need | Existing piece | Reuse |
|---|---|---|
| Working-tree capture | `WorkspaceOperations::snapshot` (`crates/envd/src/workspace/operations.rs:384`). It walks with `.hidden(true).gitignore(true).skip_git(true)` (`:776-779`). It stores every file's content in envd's SHA-256 blob store through `hash_file` (`:924-952`, with an mtime/size fingerprint cache) and writes a sorted `{path, mode, sha256}` manifest blob (`:263-272`, `:762-842`). Walker semantics: `hidden(true)` *includes* dot files (`crates/walker/src/lib.rs:616-632`). | It becomes the input set as is. Only a Merkle `Directory` builder and a secret filter are new. |
| Digests and CAS | `omp_core::Hash32` is SHA-256 (`crates/core/src/hash32.rs:22`, `:47`, `:52`). `BlobRef {hash, size}` (`crates/journal/src/blob.rs:47`). `BlobStore` `put`/`begin_put`/`open_range`/`has`/`verify` (`:368`, `:546-679`). Wire Stat/Get/Put/Delete (`crates/proto/proto/omp/blob/v1/blob.proto:15-25`). | One to one with REAPI `Digest{hash, size_bytes}` (REAPI `:1177-1183`). No new digest type. |
| Local execution | `ExecHost` (`crates/envd/src/exec.rs:288`). RAII `ExecRun` (`:236`) whose drop cancels. TERM, 250 ms grace, then KILL to recorded pgids (`:84`, `:2416-2426`). Sandbox `SpawnWrapper` (`crates/shell/src/interp.rs:147`). | The Local backend. |
| Run-handle shape | `ShellExec`/`ShellRun` (`crates/tools/src/shell.rs:332`, `:345`): zero-box, `next_event`, `cancel`, `detach`. | The backend run handle copies this shape. |
| Capability vocabulary | `ExecBackendCapabilities` (`crates/proto/proto/omp/env/v1/env.proto:1176-1185`). | Pattern for backend capabilities. |
| Build-system knowledge | Cleanse checker families: cargo check/clippy/test, go vet/test, pytest, dotnet-build, gradle-check, maven-verify, zig-build and more (`crates/driver/src/cleanse/checkers.rs:164-598`, `Checker` at `types.rs:61-83`). | The seed of the BuildSystem adapters. |
| Deterministic commands | ADW code phases: `PhaseKind::Code` "exit status decides the outcome" (`crates/adw/src/workflow.rs:29-36`), `PhaseSpec::Command{binary,args,cwd}` (`crates/driver/src/adw/definition.rs:27-43`). Posture admission: `Requirement`/`Posture`/`check` (`crates/adw/src/profile.rs:105-128`, `:231`). | The first caller, plus its admission vocabulary. |
| Config and credentials | Convars via `omp_con::var!` with flags; `PROJECT` must never be granted to "a redirected endpoint" (`crates/con/src/spec.rs:29-36`). `CredentialStore` (`crates/ai/src/auth/store.rs:414`). `CommandCredentials` (`crates/driver/src/bridges.rs:185`). `SecretString` (`crates/core/src/secret.rs:49`). | Backend endpoint and credential settings. |
| Telemetry | `vocab!` enums (`crates/observability/src/semconv.rs:59+`), `omp.tool.place` (`crates/observability/src/attrs.rs:209`). | New `omp.exec.*` attributes. |
| Native SSH | `omp ssh exec <alias> <command>` (`crates/app/src/ssh_cmd.rs:77`, `:137`) on `russh` (`Cargo.toml:207`, `crates/envd/Cargo.toml:91`). | A future SSH backend that needs no new client. |
| Toolchain at a fixed path | `fetch-python.sh <dest>` installs the CPython bundle anywhere (`crates/py/scripts/fetch-python.sh:33-41`). omp-py finds the bundle only through `PYO3_CONFIG_FILE` (`crates/py/build.rs:47-70`). The repo `[env]` entry does not override an existing environment variable (Cargo `#env`). | A remote image can carry the bundle at `/opt/omp-python`. |

**Missing:**
- an execution backend abstraction or router;
- a Merkle input tree;
- declared outputs;
- build/exec telemetry;
- a REAPI client.

**Known defects that bear on this ADR:**
- **ADW code phases run unconfined.** They spawn a bare `tokio::process::Command` with
  `kill_on_drop`, no process group and the full environment (`crates/driver/src/adw/production.rs:102-145`).
  `cleanse` does the same (`crates/driver/src/cleanse/production.rs:133-146`). This is audit F11
  (`docs/parked/2026-10-06-project-input-trust-audit.md:41`, `:48+`).
- **`omp adw run` ignores user and project cfg.** It builds `Ctx::new()` (`crates/app/src/adw_cmd.rs:55`),
  not `process_ctx_with` (`crates/app/src/lib.rs:132-147`).
- **Its cancel token is never triggered** (`adw_cmd.rs:58`).

### Requirements

- **R1. Relief first.** The first slice must measurably remove local CPU, RAM, disk or wall time
  within days, with the least new code.
- **R2. Replaceable backend.** No BuildBuddy URL, key, header, property name or proto in the domain
  layer. Swapping BuildBuddy for another REAPI server, SSH or local is configuration, not a code
  change in callers.
- **R3. BuildSystem ≠ ExecutionBackend.** An adapter knows cargo, just, maven or gradle. A backend
  knows argv, env, inputs, outputs, platform, resources and timeout.
- **R4. Working tree, not HEAD.** Uncommitted, staged, unstaged and new files are included
  deterministically. Deleted files are absent. Secrets and ignored files are never uploaded by
  default.
- **R5. Typed failures.** A command that fails is never confused with infrastructure that fails.
  Fallback never reruns a failed command elsewhere.
- **R6. Bounded and cancellable.** Output goes through the ADR 0009 spill path. Cancellation maps
  onto the omp cancellation tree. Every remote action has a timeout.
- **R7. Measurable.** Every run reports queue, upload, execute and download time, plus input/CAS
  counts, through omp's telemetry conventions.
- **R8. Polyglot.** A Maven, Gradle, Go, .NET, npm/pnpm, Python or Bazel adapter must be addable
  without touching any backend.

### Constraints

- **AGENTS.md conventions.**
  - Crates are `omp-*`, each with a README, a real description and workspace lints. A new crate
    gets one `area/*` label (`AGENTS.md:103`, `.github/labeler.yml:7-8`).
  - All dependencies are workspace dependencies. Protobufs go through `protox`, never a system
    `protoc` (`AGENTS.md:612`).
  - Errors use `thiserror` with typed sources, never string payloads (`AGENTS.md:191`).
  - Async traits are unboxed. Mailboxes are `flume`.
  - SHA-256 is `Hash32`. BLAKE3 is barred for new discretionary hashing (`AGENTS.md:127-139`).
  - "No vendor server-side tools (lock-in)" (`AGENTS.md:513`).
  - Feature graphs earn their weight, and cold `cargo run --bin omp` time is a gate
    (`AGENTS.md:536`).
- **ADR 0003.** The journal plus CAS is the only durable authority. A remote action cache is a
  cache, never a second truth.
- **ADR 0006.** Policy lives on the trusted host. The same host "MUST be able to point the stub at
  a local process, a container, a VM, or a remote machine".
- **ADR 0009, 0010, 0011.**
  - Output is bounded once, host side.
  - A remote run is a job, not a new primitive.
  - Cancellation is a runtime guarantee locally.
- **ADR 0028.**
  - The sandbox posture is enforced, not self-described.
  - Uploading project content to a third party is an egress effect.
- **ADR 0039 (proposed).**
  - Placement admits only on the *enforced* posture (B4/B5).
  - Hosts exchange SHA-256 blobs (B7).
  - Its unit is a whole session, not a command.
- **PHILOSOPHY.md.** "local, VM, remote, and headless-fleet deployments one topology" (`:59`).
  "Independence as strategy" (`:295-298`).

### BuildBuddy findings, kept separate

**1. Remote Cache (CAS + Action Cache over gRPC).**
- Endpoint `grpcs://remote.buildbuddy.io` (BB `docs/rbe-setup.md:16`).
- Auth header `x-buildbuddy-api-key` "along with all gRPCs requests" (BB `docs/guide-auth.md:11-18`).
- Digest functions SHA256, SHA384, SHA512, SHA1 and BLAKE3. The legacy single execution digest
  is SHA256 (BB `server/remote_cache/capabilities_server/capabilities_server.go:71`, `:94`).
- `max_batch_total_size_bytes = 0` ("protocol limit", `:83`).
- ZSTD only when transcoding is enabled (`:65-68`).
- Split/splice support (`:87-88`).
- Roles (BB `docs/guide-auth.md:116-136`):
  - Admin and Writer read and write CAS and AC.
  - Developer reads and writes CAS but only reads AC.
  - Read-only keys "disable remote cache uploads" (`:60-66`).
- **No HTTP or WebDAV cache endpoint is documented** in BB `docs/`, so sccache cannot target it.
  The absence is UNVERIFIED beyond the docs search.

**2. Remote Execution over raw REAPI.**
- REAPI 2.0-2.11 (`capabilities_server.go:62-63`; REAPI README `:62`).
- `Execute` checks the action cache unless `skip_cache_lookup`, merges identical in-flight
  actions, then checks a CPU quota (BB
  `enterprise/server/remote_execution/execution_server/execution_server.go:1179-1208`). None of
  that requires Bazel.
- BuildBuddy's own CLI submits arbitrary actions: `bb execute [opts] -- <executable> [args]` with
  `--input_root`, `--output_path`, `--exec_properties`, `--action_env`, `--cache_read/--cache_write`
  and `--remote_timeout` (default 1 h) (BB `cli/execute/execute.go:36-80`). `bb execute` is
  absent from the public CLI docs (BB `docs/cli-commands.md`).
- **Cancellation.** Dropping the `Execute`/`WaitExecution` stream does **not** cancel. The server
  logs "client disconnected before action completed" (`execution_server.go:1370`). No
  `CancelOperation` handler was found. Cancellation is per invocation:
  `ExecutionServer.Cancel(invocationID)` (`:2097`) and `BuildBuddyService.CancelExecutions`, which
  the `bb remote` CLI calls with the user's API key on SIGINT/SIGTERM (BB
  `cli/remotebazel/remotebazel.go:1069`, `:1366-1380`).
- **Live output.** No reference to REAPI `stdout_stream_name` (REAPI `:1744`) was found in the BB
  sources sampled. Live output for raw REAPI is UNVERIFIED, so assume logs arrive at completion.

**3. Bazel-specific.**
- `--remote_*` flags and `.bazelrc`.
- BES ingestion and the invocation UI (executions are listed per invocation, BB
  `docs/remote-runner-introduction.md:331`, `:453`).
- `bb remote <bazel command>` output fetching.
- Whether a raw-REAPI execution with no BES stream is browsable in the UI is UNVERIFIED.

**4. Remote runners (`bb remote --script`, Run API, Workflows).**
- BuildBuddy-proprietary, not REAPI. It "runs any bash commands, not just Bazel commands" (BB
  `docs/remote-bazel.md:7-13`, `:250-276`).
- The CLI mirrors local git state: `git diff --binary <base>` plus every untracked non-ignored file
  (`git ls-files --others --exclude-standard`) (BB `cli/remotebazel/remotebazel.go:539-563`).
- Warm Firecracker snapshots are keyed by instance name, platform properties, VM size and git
  branch (BB `docs/remote-bazel.md:506-520`).
- It returns the remote exit code (`remotebazel.go:1264`) and reads the key from
  `BUILDBUDDY_API_KEY`, else from `.git/config` `buildbuddy.api-key` (BB `cli/login/login.go:405-428`).
- **Friction verified in source.**
  - Even with `--script`, the CLI calls `bazel.FindWorkspaceFile(".")` and fails without a
    `WORKSPACE`, `WORKSPACE.bazel`, `MODULE` or `MODULE.bazel` file at or above the working
    directory inside the repo (`remotebazel.go:1500-1503`; BB `server/util/bazel/bazel.go:82-101`;
    `remotebazel.go:1041-1061`).
  - Private repos must be linked in BuildBuddy first (BB `docs/remote-bazel.md:352-355`).
  - Snapshot reuse needs an API key with AC write (`:461-468`).
  - Default workflow VMs have 3 CPU, 8 GB RAM and 20 GB disk (BB `docs/workflows-config.md:466-470`).
    Larger sizes are set with `EstimatedCPU`/`EstimatedMemory`/`EstimatedFreeDiskBytes`
    (`docs/remote-bazel.md:88-92`).

**5. Self-hosted executors ("Bring your own runners").**
- Available on every tier (BB `website/src/pages/pricing.tsx:196`). They need an executor key (BB
  `docs/guide-auth.md:64-66`).
- They are the only way to get `darwin` or `windows` (`docs/rbe-platforms.md:239`), and
  `host`/`docker`/`podman`/`sandbox`/`none` isolation (`:255-257`, `:361-372`).
- For relief, such an executor must be a machine *other than this laptop*.

**6. Platforms and network.**
- `OSFamily` is linux by default. `Arch` is amd64 by default or arm64 (`docs/rbe-platforms.md:238-241`).
  Managed arm64 is UNVERIFIED: `docs/remote-bazel.md:200-201` says "arm64 is supported with
  self-hosted executors".
- `network=off` is the default for OCI and "does have access to the localhost network".
  `external` is the default for Firecracker. `host` is "not available in BuildBuddy cloud"
  (`docs/rbe-platforms.md:358-372`).
- The default image is Ubuntu 16.04 (`:7-9`, `:351`). Pinned Ubuntu 24.04 is
  `gcr.io/flame-public/rbe-ubuntu24-04@sha256:f7db0d47…` (BB `server/util/platform/platform.go:38`).
- `recycle-runner`, `preserve-workspace` and `runner-recycling-key` keep a warm microVM, entire
  memory included, "an optimization, and should not be relied upon for correctness"
  (`docs/rbe-microvms.md:77-125`; `docs/rbe-platforms.md:258-269`).
- PTY availability is not documented: UNVERIFIED.

**7. Plan.**
- No BuildBuddy account, key, `bb` CLI or BuildBuddy line in `~/.bazelrc` exists on this machine
  (presence checked; no values read).
- Per `pricing.tsx`:
  - Personal is free: up to 80 Linux cores, 100 GB cache transfer, Workflows, BYO runners, no
    Mac cores (`:27-40`, `:161-201`).
  - Team: Mac cores at $45/core. Its overage rate is a literal placeholder `$X` (`:59`, `:168`).

### Options considered

Architecture:

- **A. Bazel-native (`rules_rust` + `crate_universe`).** **Rejected** (owner constraint and cost).
  - It gives per-action caching and remote execution with Bazel as the REAPI client.
  - It needs a second hand-maintained graph for 48 crates and 1315 registry packages.
    `crate_universe` generates only the external crates
    (`https://bazelbuild.github.io/rules_rust/crate_universe_bzlmod.html`).
  - It must model the vendored CPython through `PYO3_CONFIG_FILE`, protox codegen in `build.rs`,
    the Cranelift dev backend (`rust-toolchain.toml:3`) and the nightly `-Z` flags.
  - rust-analyzer, `just` and CI stay on Cargo, so developers maintain two build systems.
  - It does not move the macOS binary off the Mac either.
  - Bazel stays welcome for products *already* on Bazel (C).
- **B. REAPI-native omp executor.** **Chosen as the backend protocol.**
  - omp submits its own Actions: `GetCapabilities` → `FindMissingBlobs` →
    `BatchUpdateBlobs`/ByteStream → `Execute` (stream of LRO `Operation`) → `WaitExecution` on
    reconnect → `ActionResult` (exit code, stdout/stderr digests, `output_paths`).
  - BuildBuddy exposes every piece except `CancelOperation`; see "BuildBuddy coverage of REAPI"
    below.
- **C. Hybrid.** **Chosen as the overall shape.**
  - Bazel projects run Bazel, and Bazel's own REAPI client talks to BuildBuddy. omp never
    decomposes Bazel's actions.
  - Non-Bazel projects go through a build adapter, then the IR, then a REAPI backend.
  - Anything unsupported runs local or on an alternate backend.

Cargo route:

- **a. sccache pointed at BuildBuddy's cache.** **Rejected.**
  - sccache has no REAPI/gRPC backend. Its WebDAV backend speaks the Bazel *HTTP* cache protocol
    (SCC `README.md:35-47`, `docs/Webdav.md:1-12`).
  - BuildBuddy documents no HTTP cache.
  - The only indirect chain is sccache → a local bazel-remote → its *experimental* gRPC proxy.
    That proxy needs the backend's Remote Asset API for HTTP clients and authenticates only by
    mTLS (BR `:251-266`). Unsupported at every hop.
  - Even working, macOS-target outputs can only be shared between Macs. Links, binaries and
    proc-macros stay local (SCC `docs/Rust.md:13`), and they are the RAM-heavy part.
- **b. One coarse REAPI Action per verification command** (`just test-pkg <crate>`, clippy, a Linux
  e2e proof) over the captured working tree, in a pinned Linux image. **Chosen.**
- **c. One REAPI Action per rustc call via `RUSTC_WRAPPER`.** **Research only.**
  - Cargo still runs build scripts and links locally.
  - Proc-macros must be built for the remote host, so a macOS host with Linux workers breaks.
    Fuchsia forces local execution for exactly this case:
    `https://fuchsia.googlesource.com/fuchsia/+/main/build/rbe/rustc_remote_wrapper.py`, lines
    296-319.
  - Absolute paths defeat cross-worktree keys (`docs/audits/sccache-mac-measurement.md:95`).
  - The only Cargo client, `cargo-reapi`, is experimental and has not been validated against a
    production REAPI service (`https://github.com/TamedTornado/cargo-reapi`).

Near-term relief, ranked:

| # | Route | What leaves the Mac | New omp code | Friction | Lock-in | Verdict |
|---|---|---|---|---|---|---|
| R0 | Stop this laptop serving omp CI (`vars.MACOS_RUNNER`, runner service) | Up to all CI load (713 min over 3 days, above) | 0 | The hosted macOS job "take[s] over an hour" (`docs/parked/2026-10-06-handoff-pending-work.md:160-161`). Alternatively, split Linux-valid tests to hosted Linux. | none | Owner decision outside this ADR, probably the single largest item |
| R1 | `bb remote --script` operator trial | Linux test, clippy and Linux e2e: compile, link and run | 0 | Account; Writer key; link the private repo; untracked `MODULE.bazel` marker; VM sizing; first-run toolchain install; the Linux suite was never gated | High (Run API), but nothing in the repo depends on it | **Slice 0** |
| R2 | omp coarse REAPI Action (`omp-exec` + `omp-exec-reapi`) | Same as R1 | 2 crates + driver/app wiring | Image build and publish; credential; warm-state tuning | Low: the profile is data | **Slice 1** |
| R3 | `bb execute` per command | Same, cold unless the runner is recycled | 0 (script) | Undocumented; needs a staged clean input dir; the key goes on argv (`--remote_header`) | High | Not chosen |
| R4 | SSH to a Linux box or a second Mac (`omp ssh exec`) | Same; a Mac box can also take macOS proofs | 0 now, a backend later | Needs a machine | none | **Slice 3** backend; usable today if a box exists |
| R5 | sccache plus a remote cache | Hits only | 0 | No BuildBuddy route (a) | n/a | Rejected for BuildBuddy. Local sccache is a separate, measured-mixed decision. |
| R6 | Per-rustc `RUSTC_WRAPPER` | rlib codegen only | Large | (c) | medium | Research |
| R7 | Bazel `rules_rust` | Everything, per action | Very large | Migration | low | Rejected (A) |

## Decision

### Part A — Shape

1. **Layers.** A request flows through these layers in order. A backend MUST NOT inspect argv to
   infer a build system. An adapter MUST NOT name a backend.

   ```
   caller: ADW code phase | cleanse checker | (later) agent job
      │  intent, e.g. Verify { target: crate omp-core, kind: Test, os: Linux }
      ▼
   BuildSystem adapter: CargoJust | Maven | Gradle | Go | Dotnet | Npm | Pnpm | Python | Bazel
      │  argv, env allowlist, output paths, platform needs, placement hint
      ▼
   ExecutionPlan (IR): ExecutionRequest[] with dependencies (one request in slice 1)
      ▼
   ExecutionRouter: pure placement over backend capabilities × policy × eligibility
      ▼
   ExecutionBackend: Local (envd ExecHost) | Reapi (profile: generic | buildbuddy) | Ssh | …
      ▼
   ExecutionResult → journal entry + CAS refs inside a session; a CLI report for ADW today
   ```

2. **Unit of remote work.** It is a coarse Action: one whole verification command over a captured
   working tree. Per-rustc remote execution is not on the roadmap until slice 1 measurements and a
   proven Cargo REAPI client exist.
3. **Backends implement one trait.** Remote backends speak REAPI v2.
   - **BuildBuddy is a profile, as data:** the auth header name, the platform property names
     (`OSFamily`/`Arch` against REAPI's standard `OSFamily`/`ISA`, REAPI `platform.md:11-30`), the
     isolation and recycling properties, and the optional invocation-cancel extension.
   - No code branches on a vendor name. This is the same rule as providers-as-data
     (`AGENTS.md:507`).
4. **Bazel products** are planned by a Bazel adapter as one Local-placed request (`bazel test …`).
   Their remote offload is Bazel's own configuration. omp records the result and never wraps
   Bazel's actions.

### Part B — Domain model (crate `omp-exec`: pure, no I/O, no vendor names)

```rust
omp_core::string_id!(ExecutionId);        // minted by the router; also the REAPI tool_invocation_id
omp_core::string_id!(BackendId);          // "local", "remote", "ssh:<alias>" — operator-named

pub struct ExecutionRequest {             // cold, cloned into telemetry/results → Arc-backed fields
   pub id:        ExecutionId,
   pub intent:    Intent,                 // strum: Check | Lint | Test | Build | Proof | Other
   pub argv:      Arc<[Str]>,             // argv[0] a bare name resolved on the executor PATH; never a shell string
   pub cwd:       Str,                    // relative to the input root
   pub env:       Arc<[EnvEntry]>,        // allowlist; EnvEntry { name, value: EnvValue::Literal(Str) }
   pub inputs:    InputSet,
   pub outputs:   OutputSpec,
   pub platform:  PlatformRequirement,
   pub resources: ResourceRequirements,
   pub placement: PlacementHint,          // adapter-derived, see Part C
   pub timeout:   Duration,               // always set; becomes Action.timeout
}
pub struct InputSet { pub manifest: BlobRef, pub files: u64, pub bytes: u64, pub excluded: ExclusionCounts }
pub enum OutputSpec { Metadata, Logs, Paths(Arc<[Str]>), All }
pub struct PlatformRequirement {
   pub os: Os, pub arch: Arch,            // strum enums: Linux|Macos|Windows, X86_64|Aarch64
   pub image: Option<ImageRef>,           // digest-pinned OCI reference; None = backend default
   pub network: omp_adw::NetworkScope,    // Disabled | Scoped | Unrestricted (crates/adw/src/profile.rs:60)
   pub needs: NeedSet,                    // SparseSet<Need>: Pty, Seatbelt, Docker, MacosSdk, Gpu
}
pub struct ResourceRequirements { pub cpu_millis: u32, pub memory_bytes: u64, pub disk_bytes: u64 }
pub struct BackendCapabilities {
   pub platforms: Arc<[(Os, Arch)]>, pub needs: NeedSet, pub max_timeout: Option<Duration>,
   pub action_cache: CacheSupport, pub cancel: CancelSupport, pub max_blob_bytes: Option<u64>,
   pub enforced: omp_adw::Posture,        // what the backend enforces, never what it intends (0039 B5)
}
pub struct ExecutionPolicy { pub fallback: FallbackPolicy, pub cache: CachePolicy }
pub struct ExecutionResult {
   pub id: ExecutionId, pub backend: BackendId, pub outcome: Outcome,
   pub stdout: Option<BlobRef>, pub stderr: Option<BlobRef>, pub artifacts: Arc<[Artifact]>,
   pub timing: Timing, pub transfer: TransferCounts, pub cache_hit: bool, pub fallback: Option<FallbackCause>,
}
pub enum Outcome { Exited { code: i32 }, Signalled { signal: i32 } }   // signals: local only
pub struct Artifact { pub path: Str, pub blob: BlobRef, pub executable: bool, pub fetched: bool }

pub trait ExecutionBackend: Send + Sync {
   type Run<'a>: ExecutionRun + 'a where Self: 'a;       // impl-inferred, unboxed (AGENTS async rules)
   fn id(&self) -> &Id<str>;
   fn capabilities(&self) -> &BackendCapabilities;
   fn start<'a>(&'a self, request: &'a ExecutionRequest, cancel: CancellationToken)
      -> impl Future<Output = Result<Self::Run<'a>, ExecutionError>> + Send + 'a;
}
pub trait ExecutionRun: Send {                         // same shape as omp_tools::shell::ShellRun
   fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_;
   fn cancel(&mut self) -> impl Future<Output = CancelOutcome> + Send + '_;
}
pub fn place(request: &ExecutionRequest, backends: &[&BackendCapabilities], policy: &ExecutionPolicy)
   -> Result<Placement, Unplaceable>;                  // pure; proptested
```

- **Digests.** `BlobRef` is the digest. If `omp-exec` cannot take an `omp-journal` dependency,
  `BlobRef` *moves* into `omp-core` in the same change (rename+move, never a copy).
- **Events.** `ExecutionEvent` carries `Queued`, `Uploading{done, total}`, `Executing`,
  `Output(OutputFrame)` (local only), and `Settled(Result<ExecutionResult, ExecutionError>)`.

### Part C — Placement, eligibility, fallback, failures

1. **Eligibility is derived from the request's needs and the backend's capabilities, never declared
   by the caller.** A self-declared hint can only *narrow* placement. The same crate yields
   *different* requests per target OS.

   | Class | When |
   |---|---|
   | `LocalRequired` | Needs `Seatbelt`, `MacosSdk`, or `Pty` when no backend offers one. Builds this host's developer binary. P8-style timing recordings (`.github/workflows/p8-baseline.yml:50-52`). Release packaging. TUI real-PTY QA (ADR 0033). |
   | `RemoteRequired` | The requested platform does not exist locally: Linux verification on this Mac, including landlock/bubblewrap paths and the Linux `cfg` code that no local run exercises. |
   | `RemotePreferred` | A Linux-valid build, test or lint with no PTY whose inputs are the working tree plus the image: `just test-pkg <crate>`, clippy on Linux, P1-P5, P9-P11, `tool_sources`. |
   | `LocalPreferred` | Short commands where upload and queueing dominate. Requests whose input closure is not fully captured. |

2. **Fallback policy** (`sv_exec_policy`):

   | Policy | Remote infra failure | Command exited ≠ 0 | No backend admits | Cancelled |
   |---|---|---|---|---|
   | `local-only` (default) | n/a | report | run local | stop |
   | `local-preferred` | run local | report | run local | stop |
   | `remote-preferred` | run local unless `RemoteRequired`; record `fallback` | **report; never rerun locally** | run local unless `RemoteRequired` | stop |
   | `remote-only` | fail, typed | report | fail `Unplaceable` | stop |

3. **Failures are typed** (`thiserror`). Facts are named fields and causes are `#[source]`.
   - **Not an error:** `Outcome::Exited{code}`. A non-zero code is "command failed".
   - `ExecutionError` variants:
     - `Unplaceable{need}`
     - `Auth{backend}`, from `UNAUTHENTICATED`/`PERMISSION_DENIED`
     - `Cas{op, digest}`, including `FAILED_PRECONDITION` with `MISSING` violations
       (REAPI `:102-111`)
     - `Capacity{backend}`, from `RESOURCE_EXHAUSTED`, BuildBuddy's CPU quota, or persistent
       `UNAVAILABLE` (REAPI `:91-97`)
     - `TimedOut{limit}`, from `DEADLINE_EXCEEDED`
     - `Cancelled{remote: RemoteCancel}`
     - `Transport{source}`
     - `Infrastructure{source}`, from `INTERNAL` or a lost worker
     - `InputCapture{source}` and `InputRefused{path, rule}`
     - `OutputRetrieval{source}`
     - `Config{source}`
   - **Fallback-eligible:** `Unplaceable`, `Transport`, `Capacity`, `Infrastructure`, upload-side
     `Cas`, and `Auth` (with a loud notice).
   - **Never fallback-eligible:** `TimedOut`, `Cancelled`, `InputRefused`, `Exited`.

### Part D — Working-tree input capture

1. **The input set is envd's workspace snapshot** (`operations.rs:384`, `:762-842`). It holds:
   - tracked files at their *working-tree* content, so staged and unstaged edits are both what the
     developer sees;
   - new untracked files that are not ignored;
   - nothing for deleted files;
   - no `.git` and no gitignored paths (`target/`, `vendor/` per `.gitignore:2`, `:7`);
   - dot files.
   Index-only ("staged") capture is a later option, not the default.
2. **Default exclusions apply even to tracked or unignored files.** A match refuses the run with
   `InputRefused` unless the operator allowlists it. These defaults are needed because omp2's
   `.gitignore` has no `.env` entry. Patterns:
   - `.env` and `.env.*`
   - `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa*`, `id_ed25519*`
   - `.ssh/**`, `.aws/**`, `.gnupg/**`
   - `.netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, `.docker/config.json`, `*.kdbx`
   - `secret-placeholder.key`
   Untracked files follow `sv_exec_upload_untracked` (`non-ignored` by default, or `none`).
3. **Determinism.**
   - Paths are sorted.
   - Mode is normalized to REAPI `is_executable`.
   - No mtimes or `node_properties`.
   - Symlinks are refused in slice 1 (the tree tracks none today).
   - The manifest becomes REAPI `Directory` messages built bottom-up in canonical form, which give
     `input_root_digest`. `Command` and `Action` are blobs too.
4. **Dedupe.**
   - `FindMissingBlobs` runs over every digest.
   - Missing blobs go up through `BatchUpdateBlobs` when they fit the advertised batch limit
     (`0` is treated as 4 MiB), else through ByteStream `Write`. Upload uses zstd only when
     `GetCapabilities` advertises it.
   - The tracked tree today is 3657 files, 62.4 MB, so the first upload is at most that, and later
     uploads only changed files.
5. **Metrics per run:** files considered, files excluded per rule, bytes scanned, bytes hashed
   (fingerprint-cache misses), blobs total, CAS hits, CAS misses, blobs uploaded, bytes uploaded
   (raw and compressed), and capture/upload milliseconds.

### Part E — Outputs and artifacts

- **`OutputSpec` modes.**
  - `Metadata`: exit code, timing and digests.
  - `Logs` (default): adds stdout/stderr, fetched from their digests into the local `BlobStore`
    and bounded once through the ADR 0009 spill path.
  - `Paths`: declared `output_paths` (REAPI `:871`), fetched lazily on first access.
  - `All`: needs an explicit operator setting, because a test binary is ~650 MB.
- **Where results land.**
  - Results carry remote digests until fetched.
  - Fetched bytes live in the local CAS and are addressed as `artifact://sha256/<hex>`.
  - They are materialized under `target/omp-exec/<execution-id>/` only on request.
  - An ADW rejection's `feedback` is the bounded stdout/stderr projection, not the unbounded
    concatenation used today (`production.rs:131-141`).

### Part F — Configuration and credentials

All settings below are `flags: archive` and **never** `project`: a hostile repository must not
redirect uploads (`crates/con/src/spec.rs:29-36`). They live in user cfg (`~/.o2`).

| Convar | Default | Meaning |
|---|---|---|
| `sv_exec_policy` | `local-only` | Fallback policy (Part C) |
| `sv_exec_backend` | `local` | Name of the preferred backend |
| `sv_exec_remote_profile` | `reapi` | Data profile: `reapi` or `buildbuddy` |
| `sv_exec_remote_endpoint` | empty | `grpcs://host[:port]`; empty disables remote |
| `sv_exec_remote_instance` | empty | REAPI `instance_name`, opaque (REAPI `:225`) |
| `sv_exec_remote_credential` | empty | A credential *source*: a stored entry or a `!command` (for example `!security find-generic-password -s buildbuddy -w`), resolved through `CommandCredentials` (10 s, 1 MiB cap). Never the value. |
| `sv_exec_remote_tls_ca` | empty | Optional CA bundle; mTLS cert/key are later |
| `sv_exec_remote_image` | empty | Digest-pinned image reference |
| `sv_exec_remote_platform` | empty | Extra properties as a `Kv`, mapped by the profile |
| `sv_exec_remote_timeout_ms` | `3600000` | `Action.timeout` |
| `sv_exec_remote_concurrency` | `1` | In-flight remote actions |
| `sv_exec_remote_cache` | `off` | `off`, `read` or `read-write` (action-cache use) |
| `sv_exec_upload_exclude` | empty | Extra exclusion globs, added to the defaults |
| `sv_exec_upload_untracked` | `non-ignored` | `non-ignored` or `none` |

Credential rules:
- The credential reaches only gRPC metadata at the backend boundary, wrapped in `SecretString`.
- It is never put in argv, action env, logs, telemetry or committed config.
- Tracing redacts the header.
- Using `bb login` is discouraged for omp checkouts, because it writes the key into `.git/config`,
  where agent tools can read it (BB `cli/login/login.go:33`, `:222`).

### Part G — Platforms and toolchain

- **Platform scope.** Linux x86_64 is first. `Os`/`Arch` leave room for arm64, macOS and Windows;
  only the profile maps them. A macOS request is `Unplaceable` unless a backend advertises
  `darwin` (a self-hosted executor or a second Mac over SSH).
- **The image is versioned with the repo** (slice 1 adds the recipe). It contains:
  - Ubuntu 24.04;
  - rustup with `nightly-2026-08-08` and the four components from `rust-toolchain.toml:2-3`;
  - cargo-nextest 0.9.146 (the CI pin, `ci.yml:304`; minimum 0.9.143 at `.config/nextest.toml:17`);
  - `just`, cmake, ninja (forced by `.cargo/config.toml:16`), a C compiler, git, python3, uv,
    zstd, curl and rsync;
  - the CPython bundle from `crates/py/scripts/fetch-python.sh /opt/omp-python`, with
    `PYO3_CONFIG_FILE=/opt/omp-python/python/pyo3-config.txt` set in the action env.
  The repo's relative `[env]` value yields to that (Cargo `#env`), and omp-py derives every path
  from it (`crates/py/build.rs:47-70`). No lld or protoc is needed on Linux.
- **The image digest is part of the platform, and so of the action key.** It changes whenever
  `rust-toolchain.toml`, the fetch script's `TAG`/`VER` (`fetch-python.sh:27-28`) or the nextest
  pin change.
- **Warm state is an optimization, never a correctness input.**
  - Properties: `workload-isolation-type=firecracker`, `recycle-runner=true`, and
    `runner-recycling-key=<image digest>`.
  - The command `rsync -a --checksum`s the input root into a fixed in-VM path and builds there,
    with `CARGO_TARGET_DIR` outside the workspace. Fixed paths keep cargo fingerprints stable
    across runs [I]. `-Zchecksum-freshness` is already on (`.cargo/config.toml:53`), though build
    scripts still use mtimes (Cargo `#checksum-freshness`).
  - Network is `external` under Firecracker. `Cargo.lock` checksums pin every registry crate.
- **A hermeticity gap to close.** `fetch-python.sh` does not verify the upstream `SHA256SUMS` of the
  CPython archive (`fetch-python.sh:53`, `:70`).

### Part H — Security model

1. **The remote CAS and the executor are a trust boundary.** Everything uploaded is disclosed to
   the provider. Enabling a remote backend is an operator act through user cfg. Every result
   records the backend id and endpoint host.
2. **Defaults block the usual leaks.**
   - Secret-pattern exclusions (Part D).
   - An action env built from an allowlist, never by inheriting the host environment. ADW code
     phases inherit everything today (F11).
   - No credentials in actions.
3. **Logs and outputs that come back are untrusted data**, like any tool output. They pass the ADR
   0009 bound and the `omp-secrets` redaction at presentation. Downloaded outputs are never
   executed locally unless a caller asks.
4. **Admission is by enforced posture** (`Requirement::check`, `crates/adw/src/profile.rs:231`).
   - A remote Linux action cannot write the local workspace at all, so the local
     `WriteScope` is `ReadOnly`.
   - Its network scope is the one its platform enforces (`off` → `Disabled`, `external` →
     `Unrestricted`).
   - Agent-initiated remote runs (a later slice) ask through the 0028 fetch/network tier, keyed by
     the endpoint host.
5. **Repo-planted binaries never resolve remotely.** argv[0] resolves on the executor PATH inside
   the image. Locally, `FilesystemResolver`'s project-first lookup
   (`crates/driver/src/cleanse/checkers.rs:30-60`) keeps applying only to local runs.

### Part I — Cancellation

| omp event | Local backend | REAPI (generic profile) | BuildBuddy profile |
|---|---|---|---|
| Token fires: turn interrupt, job cancel, or `omp adw run` Ctrl-C (to be wired) | TERM to pgids, 250 ms grace, KILL (`exec.rs:84`, `:2416-2426`) | Stop waiting. Call LRO `CancelOperation` if capabilities say so (best-effort, LRO `:88-99`). Settle `Cancelled{remote: Requested\|Unsupported}`. | `CancelExecutions(invocation_id)` as `bb remote` does. Dropping the stream alone leaks the action until `Action.timeout`. |
| Timeout | Local timeout | `Action.timeout`, always set (REAPI `:715`) | Same, plus `termination-grace-period` |
| Disconnect, sleep or crash | n/a | Reattach with `WaitExecution(name)` (REAPI `:136`) | Same |

- **Each `ExecutionId` is the REAPI `tool_invocation_id`** in `RequestMetadata` (REAPI `:2622`), so
  a cancel touches only this run.
- **Guarantees.**
  - Cancellation is guaranteed *locally*: results are never applied after cancel.
  - Remotely it is best-effort and bounded by `Action.timeout`.
  - It is reported as `EffectsUnknown`-style truth, never as "stopped".
- **Duplicate executions.** REAPI lets a server run an action more than once (`:113-116`), so
  actions MUST be idempotent with respect to the outputs they declare.

### Part J — Telemetry

The span is `omp.exec.execute`, with children `capture`, `upload`, `queue`, `run` and `download`.
Remote phases come from `ExecutedActionMetadata` (queued, worker start, input fetch, execution and
output upload timestamps; REAPI `:1187-1235`).

Attributes (new `vocab!` enums):
- `omp.exec.id`
- `omp.exec.backend`
- `omp.exec.placement`
- `omp.exec.intent`
- `omp.exec.outcome`
- `omp.exec.exit_code`
- `omp.exec.failure`
- `omp.exec.fallback`
- `omp.exec.cache_hit`
- `omp.exec.input.files`, `omp.exec.input.bytes`
- `omp.exec.cas.hits`, `omp.exec.cas.misses`
- `omp.exec.upload.blobs`, `omp.exec.upload.bytes`
- `omp.exec.download.bytes`
- `omp.exec.endpoint.host`

Metrics:
- `omp.exec.duration`, a histogram by backend and outcome;
- upload and CAS counters;
- fallback counts.

Headers, credential values and env values are never recorded.

### Part K — CLI and UX

**No `omp build` verb.** Two existing verbs already mean "run the project's deterministic
verification":

- **`omp adw run <workflow>` code phases** (`crates/app/src/cli.rs:1037-1058`). Code phases are
  deterministic commands whose exit status decides (`workflow.rs:29-36`). A checked-in
  `.omp/workflows/verify.toml` with `command = ["just", "test-pkg", "omp-core"]` is the slice 1
  entry point.
- **`omp cleanse`** (`cli.rs:1166-1167`; families at `checkers.rs:164-200`) in slice 2.

Placement comes from the convars, and `omp adw run --exec-policy <p>` overrides them for one
process. Output adds one line per phase, for example
`remote (reapi remote.example:443) 4m12s: queue 3s, upload 1.2 MB / 41 blobs, exit 0`.
For `omp adw run` to honor cfg at all, it must use `process_ctx_with`.

### Part L — Recommended slices

- **Slice 0: operator trial, no product code, this week. The recommended first slice.**
  1. Create a BuildBuddy account and a Writer-role key. Store the key in the macOS Keychain.
  2. Install `bb`.
  3. Link `blyzer/oh-my-pi` if it is private.
  4. Create an empty `MODULE.bazel` at the repo root, listed in `.git/info/exclude`. It is never
     committed, and git mirroring skips excluded files.
  5. Set `GIT_REPO_DEFAULT_BRANCH=omp2`. The CLI otherwise patches against the default branch
     (`docs/remote-bazel.md:173-185`), which is v1 `main`.
  6. Check that `git ls-files --others --exclude-standard` lists no secrets.
  7. Run:
     `BUILDBUDDY_API_KEY="$(security find-generic-password -s buildbuddy -w)" bb remote --os=linux --arch=amd64 --runner_exec_properties=EstimatedCPU=16 --runner_exec_properties=EstimatedMemory=32GB --runner_exec_properties=EstimatedFreeDiskBytes=100GB --timeout=60m --script='<install toolchain if absent>; crates/py/scripts/fetch-python.sh; just test-pkg omp-core'`
     The toolchain install persists in the VM snapshot after the first run.
  8. Measure as in "Measurement plan".

  This gives relief on the first warm run. It answers the open questions (UI visibility, VM
  sizing, PTY, nested sandboxes, Linux failures) before any omp code exists. It is disposable:
  nothing in the repo depends on `bb`.
- **Slice 1: the first product code.** A remote backend for ADW code phases. Files are listed under
  "Implementation plan".
- **Slice 2:**
  - the Local backend through envd `ExecHost`, which closes F11 for routed phases;
  - the fallback policy;
  - `cleanse` checkers through the router;
  - output modes;
  - the BuildBuddy invocation-cancel extension.
- **Slice 3:** an SSH backend over `russh` (`omp ssh` hosts). It is the cheapest proof that
  BuildBuddy is replaceable, and it can reach a second Mac for macOS proofs.
- **Later:**
  - agent-initiated remote runs as jobs (ADR 0010), journaled (ADR 0003);
  - action-cache verdict reuse;
  - in-flight dedup (parked P15 note);
  - more build-system adapters.

## Consequences

- **Easy:**
  - Linux verification leaves the Mac without a second build graph.
  - Any REAPI server, an SSH host or the local machine is a configuration change.
  - New language adapters never touch backends.
  - Isolated subagent worktrees, which are cold today because gitignored `target/` is not copied
    (ADR 0007 status), can share one warm remote runner.
  - The Linux workspace suite, gated nowhere today, gets a place to run.
- **Prohibited:**
  - a Bazel migration as a prerequisite;
  - vendor names, URLs, headers, properties or protos in `omp-exec` or callers;
  - omp product code shelling out to `bb`;
  - project-scoped backend or endpoint convars;
  - credentials in argv, action env or committed files;
  - rerunning a failed command on another backend;
  - treating a remote action cache as truth;
  - per-call boxing in the backend path;
  - BLAKE3 digests.
- **Costs accepted:**
  - an image to build, publish and keep in step with the toolchain pins;
  - remote runs that are cold unless a runner is recycled;
  - logs that arrive at completion, not live (UNVERIFIED either way);
  - source disclosure to the chosen provider;
  - macOS-only work that stays on a Mac;
  - two new crates and vendored REAPI/googleapis protos;
  - a measurable addition to cold `omp` build time, to be measured against the `AGENTS.md:536`
    gate.

## Status in omp

**Status: Not started.** Proposed 2026-10-08 against `omp2` at `929f902235`. None of the components
exist: no `omp-exec`, no REAPI client, no router, no `sv_exec_*` convars and no `omp.exec.*`
telemetry. The reusable pieces are listed in Context.

### BuildBuddy coverage of REAPI

| REAPI surface | Standard (REAPI line) | BuildBuddy | Notes |
|---|---|---|---|
| Capabilities | `:646`, `ServerCapabilities` `:2206` | yes | API 2.0-2.11, digest list, `update_enabled` by role |
| CAS `FindMissingBlobs`, `BatchUpdate/ReadBlobs`, `GetTree` | `:360-439` | yes | Batch limit "protocol limit" |
| ByteStream incl. `compressed-blobs/zstd` | `:213-292` | yes | ZSTD only if enabled; read capabilities |
| Split/Splice | `:448`, `:532` | advertised | Not needed |
| Action cache `Get/UpdateActionResult` | `:170`, `:193` | yes | Writer/Admin to write |
| `Execute` / `WaitExecution` (LRO stream) | `:118`, `:136` | yes | Merges identical in-flight actions |
| `CancelOperation` | `:99-101`, LRO `:88-99` | not found | Use invocation cancel (BuildBuddy extension) |
| `Action`/`Command`/`Directory`/`Digest`/`Platform` | `:674`, `:749`, `:1048`, `:1177`, `:934` | yes | Properties in BuildBuddy vocabulary |
| `output_paths` | `:871` | yes | — |
| stdout/stderr digests, exit code, `server_logs` | `ActionResult` `:1250+` | yes | Live streams UNVERIFIED |
| `Action.timeout`, `do_not_cache`, `salt` | `:715`, `:719`, `:729` | yes | Cloud max timeout UNVERIFIED |
| `RequestMetadata.tool_invocation_id` | `:2622` | yes | Keys cancellation and the UI |

### Answers to the owner's ten questions (2026-10-08)

1. **Can current Cargo compilation run remotely without a Bazel migration?**
   - **Yes, for Linux targets, as whole commands.** One Action runs `just test-pkg <crate>` or
     clippy over the captured tree.
   - **No for the macOS developer binary.** There are no managed darwin executors, and the link
     needs the macOS SDK and Homebrew lld. Apple SDK licensing on non-Apple hardware was not
     researched.
2. **How?**
   - **Now:** `bb remote --script` (Slice 0).
   - **Owned:** a coarse REAPI Action from `omp-exec-reapi` in a digest-pinned Linux image, with
     `PYO3_CONFIG_FILE` pointing at a baked CPython and a recycled Firecracker runner for warm
     `target/` (Slice 1).
3. **Smallest valuable integration now:** Slice 0. It is zero code, and it measures relief before
   any code is written.
4. **Is BuildBuddy's direct REAPI suitable for arbitrary build/test commands?** Yes. `bb execute`
   is the proof. Caveats:
   - no stream-drop cancellation;
   - OCI runs with `network=off`;
   - whole-action cache granularity;
   - live logs, PTY, UI visibility and the cloud maximum timeout are UNVERIFIED.
5. **Bazel-specific vs standard REAPI:** see "BuildBuddy findings" 1-4.
   - **Standard REAPI:** CAS, AC, Execute/WaitExecution, ByteStream, capabilities, RequestMetadata.
   - **BuildBuddy-only:** the API-key header, `x-buildbuddy-platform.*` overrides, the property
     vocabulary, invocation cancel, the Run API, `bb remote`, Workflows, BES and the UI.
   - **Bazel-only:** `.bazelrc` flags and `bb remote <bazel cmd>` output fetching.
6. **Auth model.**
   - An org or personal API key in `x-buildbuddy-api-key` on every gRPC call.
   - Roles gate AC writes. Read-only and executor keys exist.
   - mTLS exists, but how Cloud issues client certificates is UNVERIFIED.
   - omp keeps the key in the Keychain or the credential store and injects it as metadata only.
7. **Capabilities on the current plan.** There is no account yet. Personal (free) per
   `pricing.tsx`: 80 Linux cores, 100 GB cache transfer, Workflows and BYO runners, no Mac cores.
   Unknown without an account:
   - the cloud message-size limit;
   - the maximum action timeout;
   - whether ZSTD is on;
   - managed arm64;
   - whether snapshot storage counts toward cache transfer;
   - quota numbers;
   - whether `CancelExecutions` accepts a key outside the CLI flow (the CLI uses one).
   A single `GetCapabilities` call plus one probe Action settles most of these.
8. **Linux limitations.**
   - amd64 by default; managed arm64 is UNVERIFIED.
   - The default image is 16.04, so a custom image is mandatory.
   - OCI has `network=off`; Firecracker has `external`; `host` is unavailable.
   - PTY is UNVERIFIED, so P6/P7 may not run.
   - Nested bubblewrap, landlock and docker tests are UNVERIFIED. Firecracker can run `dockerd`.
   - macOS `cfg` code is not exercised.
   - The Linux workspace suite has never been gated, so expect pre-existing failures.
   - Default workflow VMs (3 CPU / 8 GB / 20 GB) are too small for an 18 GB test target.
9. **What needs self-hosted executors:**
   - `darwin`: P12, P7's macOS case, the Seatbelt tests, macOS `cfg` coverage, release
     packaging;
   - `windows`;
   - possibly arm64 Linux;
   - `host`/`docker`/`podman`/`sandbox`/`none` isolation;
   - custom hardware.
   For relief, these must run on a machine other than this laptop.
10. **What stays portable to another REAPI provider:**
    - `omp-exec` (domain, router, placement, failures);
    - input capture and the Merkle builder;
    - the REAPI client and the `reapi` profile;
    - the output, telemetry and cancellation models.
    Not portable: the `buildbuddy` profile data (header, property names, recycling and isolation
    properties), invocation cancel, and Slice 0's `bb remote` usage.

### Implementation plan (PR-sized, value first)

- **Slice 0: no repository change** except, afterwards, `docs/audits/remote-verification-trial.md`
  recording the measurements.
- **Slice 1, PR 1a: `crates/exec` (`omp-exec`, new).**
  - Contents: domain types (Part B), the pure `place()` function, `ExecutionError`, default
    exclusion globs, and strum enums with `vocab!` names for telemetry.
  - Also: a README, `[lints] workspace = true`, and an `area/env` entry in
    `.github/labeler.yml`.
  - Dependencies: `omp-core`, `omp-adw` (for `NetworkScope`/`Posture`), `thiserror`, `strum`,
    `tokio-util` (the token).
  - Tests: unit tests plus a proptest over placement × policy × failure.
- **Slice 1, PR 1b: `crates/exec-reapi` (`omp-exec-reapi`, new).**
  - Protos are vendored under `crates/exec-reapi/proto/`:
    - REAPI v2 `remote_execution.proto`;
    - `semver.proto`;
    - googleapis `bytestream`, `longrunning/operations`, `rpc/status`, `rpc/error_details`, and the
      `api` annotations they import.
  - `build.rs` compiles them with protox plus tonic-prost-build, like `crates/proto/build.rs`.
  - Modules:
    - `profile`: `reapi` and `buildbuddy` as data;
    - `tree`: manifest to canonical `Directory`;
    - `cas`: FindMissing, batch, ByteStream, zstd;
    - `execute`: Execute, the WaitExecution reattach, and result fetch;
    - `capabilities`.
  - The backend reads blob bytes through a `BlobSource` trait implemented over envd's blob service,
    so it holds no second copy.
  - License review of the vendored protos is part of the PR. Their licenses were not checked in
    this pass (UNVERIFIED).
- **Slice 1, PR 1c: wiring.**
  - `crates/driver/src/exec/{mod.rs,settings.rs}` (new): `sv_exec_*` convars (Part F), backend
    composition, and credential resolution through `CommandCredentials`.
  - `crates/driver/src/adw/production.rs`: `run_command` builds an `ExecutionRequest` and routes
    it; the local path stays as is until slice 2.
  - `crates/app/src/adw_cmd.rs`:
    - use `process_ctx_with` (cfg finally applies);
    - wire Ctrl-C to the cancel token;
    - add `--exec-policy`;
    - print the placement line.
  - `crates/observability/src/{semconv.rs,attrs.rs}`: `omp.exec.*`.
  - `scripts/exec-image/` (new): the image recipe and a build script that derives pins from
    `rust-toolchain.toml`, `fetch-python.sh` and `ci.yml`.
  - `.omp/workflows/verify.toml` (new): `just test-pkg` and clippy phases.
- **Slice 2:**
  - the Local backend over envd `ExecHost`;
  - fallback;
  - `crates/driver/src/cleanse/production.rs` routed;
  - output modes;
  - the BuildBuddy `CancelExecutions` extension in `omp-exec-reapi`.
- **Slice 3:** an SSH backend in `crates/envd` (it owns `russh`) behind the same trait.

### Test plan (owning seams)

- **`omp-exec`.**
  - A placement proptest:
    - `LocalRequired` never goes remote;
    - `RemoteRequired` never goes local;
    - an exit ≠ 0 never falls back;
    - `Cancelled` never falls back.
  - A failure-classification table test from every gRPC code in REAPI `:87-101`.
- **`omp-exec-reapi`.**
  - Golden `Directory`/`Action` digests against REAPI canonical-encoding vectors.
  - A **fake in-process REAPI server** (tonic) with CAS, AC, Execute and WaitExecution, plus
    injectable faults:
    - a `MISSING` precondition;
    - `UNAVAILABLE`;
    - `RESOURCE_EXHAUSTED`;
    - a stream drop with reattach;
    - `DEADLINE_EXCEEDED`;
    - no cancel support.
  - Bounded waits; RAII-owned servers.
- **`omp-driver`.** An ADW code phase routed to the fake backend:
  - exit 0 accepts;
  - exit 1 rejects with bounded feedback;
  - an infra fault plus `remote-preferred` gives a local fallback;
  - an infra fault plus `remote-only` gives a typed failure;
  - Ctrl-C produces `Cancelled` and a cancel request.
- **`omp-envd`.** Secret-exclusion refusals on a snapshot fixture: a tracked `.env`, an untracked
  key file.
- **Live tests** are gated behind `OMP_EXEC_LIVE_ENDPOINT` plus `OMP_EXEC_LIVE_CREDENTIAL` (a
  credential source, never a value) and skipped otherwise. They never run in CI by default.

### E2E proofs to add

- **P13: remote verification over a fake REAPI server.**
  - `omp adw run` with a remote-placed code phase.
  - It proves: capture, dedupe on the second run, result, cancel, and no secret uploaded.
- P13 is the next free id; ADR 0039's planned P11/P12 numbers were reused (`crates/e2e/tests/`).

### Measurement plan

The method follows the audits:
- interleaved arms;
- at least 3 repetitions;
- crates fetched once, untimed;
- no cache priming or purging to flatter an arm: cache state is recorded, not manipulated.

What is measured:

- **Local baseline per recipe** (`just test-pkg omp-core`, `just test-pkg omp-envd`, `just clippy`,
  `just e2e-build`):
  - `/usr/bin/time -l`: wall, user+sys CPU, maximum resident set size;
  - the swap-in delta from `vm_stat`;
  - the `target/` size delta;
  - with the CI runner service idle and recorded.
- **Remote arms:**
  - cold: a new snapshot key or fresh runner;
  - warm: a recycled runner;
  - action-cache hit: an identical action with AC read.
- **Per remote run:** local capture, FindMissing, upload bytes and time, queue, input fetch,
  execution, output upload, download (from `ExecutedActionMetadata`), plus local CPU and peak RSS
  of the client.
- **Success criterion for Slice 0/1:** local wall and peak RSS of the client are each under 5% of
  the local recipe. Warm remote wall time is within 1.5× of warm local. Both thresholds are owner
  adjustable.
- **Result record:** `docs/audits/remote-verification-trial.md`.

### Relationship to P14, P15, P16 and other ADRs

- **P14, P15 and P16 are not repo artefacts.** They are tracks defined only in the parked,
  non-authoritative note `docs/parked/2026-10-06-p14-p16-architecture-notes.md:1-5`. `PLAN.md`
  and `.plan/` are absent from this checkout.
- **P16 (Build/Execution).** This ADR is the omp-owned form of "P16.8 BuildBuddy/Bazel remote path"
  (`:542-554`). It **replaces** the parked sketch "Bazel → BuildBuddy remote cache + remote
  execution" (`:500-509`) with "no Bazel; REAPI behind an adapter". It keeps that note's principles:
  - planner ≠ executor ≠ cache (`:59-86`);
  - bypass rather than guess;
  - an explicit adapter, never "mbx → BuildBuddy" (`:511-515`).
  Its metrics list (`:556-570`) seeds the measurement plan.
- **P15 (orchestration).** In-flight dedup and checking the cache before taking a permit
  (`:90-140`, `:338-362`) are a later router feature.
- **P14 (review semantics).** No relationship.
- **ADR 0039.**
  - A fleet worker (K2) may use this router for its own commands.
  - Both share enforced-posture admission (B5) and SHA-256 blob exchange (B7).
  - This ADR places commands; 0039 places sessions.
- **ADR 0007.** Remote warm runners mitigate the cold builds of isolated children.

### Future polyglot products

Each adapter is data plus argv shaping in the BuildSystem layer, seeded by the cleanse families.
None touches a backend.

| Product | Adapter emits | Remote prerequisites (image) | Notes |
|---|---|---|---|
| Maven / Gradle | `mvn -B verify`, `gradle check` | JDK plus a prefetched or `network=external` repository | Gradle has its own remote build cache. That is the build tool's choice, like Bazel. |
| Go | `go vet ./...`, `go test ./...` | Go toolchain and module cache | — |
| .NET | `dotnet build`/`test` | SDK | Windows targets need self-hosted executors |
| npm/pnpm | `pnpm -r test` | Node and the store | `node_modules/` is excluded from inputs, so the action installs from the lockfile |
| Python | `uv run pytest` | `uv`, interpreter | — |
| Bazel | `bazel test //...`, Local-placed | n/a | Bazel offloads itself (Part A.4) |

### Risks

- **Disclosure.** Source and any mis-excluded secret reach a third party. Mitigations: the default
  deny list, untracked-file policy, operator-only enablement, and pre-run listing in Slice 0.
- **Linux failures that are not offload failures.** The Linux suite was never gated (`ci.yml:338`
  is macOS only).
- **Warm state does not hold.** Runner recycling is best-effort, so remote cold runs at default
  sizes can be slower than local.
- **Leaked remote work after cancel** on BuildBuddy unless invocation cancel is used. It is
  bounded by `Action.timeout`.
- **Cost and quota surprises.** Team pricing placeholders; snapshot storage accounting is unknown.
- **Image drift from the pins.** Mitigation: the image is derived from the repo files and its
  digest is in the action key.
- **The largest load source may be CI on this laptop (R0),** which no BuildBuddy slice fixes.

### Not verified

- Whether `vars.MACOS_RUNNER` still targets this laptop.
- Peak RSS and swap of any recipe.
- BuildBuddy Cloud specifics:
  - message-size limit;
  - maximum action timeout;
  - whether ZSTD is enabled;
  - managed arm64;
  - PTY;
  - nested sandboxes;
  - UI visibility of raw REAPI;
  - live stdout streams;
  - `CancelOperation`;
  - `CancelExecutions` outside the CLI;
  - whether input mtimes are preserved;
  - whether the workspace path is stable across recycled runs;
  - snapshot storage accounting;
  - Team prices.
- Whether `bb remote --container_image` accepts the `ubuntu-24.04` alias.
- Pants or Reclient on BuildBuddy.
- The licenses of the vendored protos.
- Apple SDK licensing for cross-builds.

### Open questions for the owner

1. **R0.** Should this laptop keep serving omp CI, and the two other runner services? It is likely
   the largest load.
2. **Disclosure.** Is uploading the omp2 source to BuildBuddy Cloud acceptable, and is linking the
   private repo to BuildBuddy acceptable for Slice 0?
3. **Image hosting.** Where is the image hosted (public registry or private with registry
   credentials), and who rebuilds it?
4. **Scope of Linux remote verification.** Should it become a CI gate (closing the `AGENTS.md:86`
   discrepancy), or stay a developer tool?
5. **Budget.** Is Personal-tier capacity (80 cores, 100 GB transfer) enough, or Team?

## References

- ADRs 0001, 0003, 0006, 0007, 0009, 0010, 0011, 0028, 0033, 0035, 0039.
- Code:
  - `crates/envd/src/workspace/operations.rs`
  - `crates/envd/src/exec.rs`
  - `crates/driver/src/adw/production.rs`
  - `crates/driver/src/cleanse/checkers.rs`
  - `crates/adw/src/profile.rs`
  - `crates/con/src/spec.rs`
  - `crates/journal/src/blob.rs`
  - `crates/core/src/hash32.rs`
  - `crates/py/scripts/fetch-python.sh`
  - `crates/py/build.rs`
  - `.cargo/config.toml`
  - `justfile`
  - `.github/workflows/ci.yml`
- Audits and notes:
  - `docs/audits/cranelift-panic-cleanup.md`
  - `docs/audits/cranelift-panic-cleanup-mac-followup.md`
  - `docs/audits/sccache-mac-measurement.md`
  - `docs/parked/2026-10-06-p14-p16-architecture-notes.md`
  - `docs/parked/2026-10-06-project-input-trust-audit.md`
  - `docs/parked/2026-10-06-handoff-pending-work.md`
- External:
  - the pinned sources in Context;
  - `https://www.buildbuddy.io/docs/remote-bazel/`
  - `https://www.buildbuddy.io/docs/rbe-platforms/`
  - `https://www.buildbuddy.io/docs/guide-auth/`
  - `https://buck2.build/docs/users/remote_execution` (Buck2 against BuildBuddy)
  - `https://github.com/bazelbuild/remote-apis` (client and server lists)
