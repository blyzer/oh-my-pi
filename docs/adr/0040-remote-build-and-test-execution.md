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
   own target dir (`justfile:61-67`). Every test binary of a crate downstream of `omp-py`
   statically links the ~650 MB CPython (`.github/workflows/ci.yml:244-249`). The direct dependents
   are `omp-app`, `omp-envd`, `omp-tools` and `omp-e2e`; `omp-driver` and `omp-chat` link it
   through them (their `Cargo.toml`). Crates such as `omp-core` do not. sccache cannot cache crates
   that invoke the system linker: `bin`, `dylib`, `cdylib` and `proc-macro` (SCC `docs/Rust.md:11`).
3. **This laptop cannot produce Linux results, and Linux executors cannot produce its macOS
   binary.**
   - On the Personal (free) tier and in the docs, BuildBuddy Cloud's managed executors are Linux;
     `darwin` and `windows` are self-hosted only (BB `docs/rbe-platforms.md:239`;
     `docs/remote-bazel.md:199`).
   - BuildBuddy's own sources also advertise **managed Mac executors on paid tiers**: the pricing
     table lists "Mac cores" at $45/core for Team and "Unlimited" for Enterprise, with none for
     Personal (BB `website/src/pages/pricing.tsx:166-169`). The v2.7.0 release notes name
     "BuildBuddy managed Mac executors" (BB `website/blog/buildbuddy-v2-7-0-release-notes.md:20`).
     The docs route them through sales: "Mac executors are not included in BuildBuddy Cloud's
     free-tier offering… contact our sales team" (BB `docs/troubleshooting-rbe.md:31-33`), and
     "contact us… about BuildBuddy-managed Macs" (BB `docs/remote-runner-introduction.md:414-417`).
     Whether a Team account can buy them self-serve, and on what terms, is UNVERIFIED.
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
| Linux reference host (4 vCPU, 15 GiB) | Cold `omp` 405.8 s (target dir 5.1 GB on disk). Cold e2e test build 427.6 s (target dir 7.3 GB). Cold workspace test build 656 s (target dir 18 GB). The GB figures are disk, not RAM. | `docs/audits/cranelift-panic-cleanup.md:8`, `:486-508` |

**Observed on the owner's machine (2026-10-08).** This is not repository evidence.

- **The machine.** `sysctl` reports `MacBookPro18,3`, 16 GB, 10 CPUs. That is the self-hosted CI
  runner model named in both audits.
- **CI runs on it.**
  - A GitHub Actions runner service for `blyzer/oh-my-pi` is installed and listening.
  - Counting method: one `~/actions-runner-omp/_diag/Worker_2026100[345]*` log per job (the file
    name carries the job's start date, UTC), named by its `jobDisplayName`, with duration taken
    from the log's first to last timestamp, so setup and cleanup are included.
  - That gives 57 `Rust workspace and acceptance proofs` jobs: median 14.2 min, p90 about 21 min,
    879 min in total, longest 120 min. The Rust jobs alone used 495 min of the machine on
    2026-10-05, and all omp jobs together used 555 min that day (979 min over the three days).
  - The same logs show 29 `Record P8 baseline` jobs (median 2.4 min) and one packaging job
    (30 min).
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
| Working-tree capture | `WorkspaceOperations::snapshot` (`crates/envd/src/workspace/operations.rs:384`) and the private `snapshot_at` it calls (`:762`). The walk uses `.hidden(true).gitignore(true).skip_git(true)` (`:776-779`). Each file's content goes into envd's SHA-256 blob store through `hash_file` (`:924-952`, with an mtime/size fingerprint cache), and a sorted `{path, mode, sha256}` manifest blob is written (`:263-272`, `:762-842`). Walker semantics: `hidden(true)` *includes* dot files (`crates/walker/src/lib.rs:616-632`). `use_gitignore` honours `.ignore`, `.gitignore`, the repo excludes and the global excludes (`crates/walker/src/lib.rs:514`, `:3122-3175`). Two consequences: tracked files that match an ignore rule are dropped, and `.ignore` files, which git does not read, also drop files. Symlinks are skipped silently (`FollowLinks::Never`, `crates/walker/src/lib.rs:550`; the `FileType::File` filter at `operations.rs:794`). The public `snapshot()` takes the workspace `transition.try_lock()` and durably publishes a generation and a snapshot record on every call (`:384-396`, `:866-905`). | Reuse the hashing, the blob store and the manifest format, **not** the walk or the publish. The input set is defined by git semantics (Part D). Capture is a new capture-only entry point next to `snapshot_at` that takes no transition lock and publishes nothing. |
| Git path sets | `omp-vcs` `ls_files(others, exclude_standard)` lists tracked paths, or untracked paths with standard excludes, in process (`crates/vcs/src/lib.rs:224`). `omp-envd` already depends on it (`crates/envd/Cargo.toml:64`). | Source of the tracked and untracked path lists for capture. |
| Digests and CAS | `omp_core::Hash32` is SHA-256 (`crates/core/src/hash32.rs:22`, `:47`, `:52`). `BlobRef {hash, size}` (`crates/journal/src/blob.rs:47`). `BlobStore` `put`/`begin_put`/`open_range`/`has`/`verify` (`:368`, `:546-679`). Wire Stat/Get/Put/Delete (`crates/proto/proto/omp/blob/v1/blob.proto:15-25`). | One to one with REAPI `Digest{hash, size_bytes}` (REAPI `:1177-1183`). No new digest type. |
| Local execution | `ExecHost` (`crates/envd/src/exec.rs:288`). RAII `ExecRun` (`:236`) whose drop cancels. TERM, 250 ms grace, then KILL to recorded pgids (`:84`, `:2416-2426`). Sandbox `SpawnWrapper` (`crates/shell/src/interp.rs:147`). | The Local backend. |
| Run-handle shape | `ShellExec`/`ShellRun` (`crates/tools/src/shell.rs:332`, `:345`): zero-box, `next_event`, `cancel`, `detach`. | The backend run handle copies this shape. |
| Capability vocabulary | `ExecBackendCapabilities` (`crates/proto/proto/omp/env/v1/env.proto:1176-1185`), answered by envd (`crates/envd/src/exec.rs:996`, `:1044`). | Pattern for backend capabilities. |
| Build-system knowledge | Cleanse checker families: cargo check/clippy/test, go vet/test, pytest, dotnet-build, gradle-check, maven-verify, zig-build and more (`crates/driver/src/cleanse/checkers.rs:164-598`, `Checker` at `types.rs:61-83`). | The seed of the BuildSystem adapters. |
| Deterministic commands | ADW code phases: `PhaseKind::Code` "exit status decides the outcome" (`crates/adw/src/workflow.rs:29-36`), `PhaseSpec::Command{binary,args,cwd}` (`crates/driver/src/adw/definition.rs:27-43`). Posture admission: `Requirement`/`Posture`/`check` (`crates/adw/src/profile.rs:105-128`, `:231`). | The first caller, plus its admission vocabulary. |
| Config and credentials | Convars via `omp_con::var!` with flags; `PROJECT` must never be granted to "a redirected endpoint" (`crates/con/src/spec.rs:29-36`). `CredentialStore` (`crates/ai/src/auth/store.rs:414`). `CommandCredentials` (`crates/driver/src/bridges.rs:185`) "runs `!command` credential sources inside the project Environment" and needs an `EnvClient` (`:181-198`). `SecretString` (`crates/core/src/secret.rs:49`). Secret content rules: `omp_secrets::credential_rules()` (`crates/secrets/src/builtins.rs:18`). | Backend endpoint and credential settings; a content scan before upload. `ProductionAdwHost` holds no envd environment today (`crates/driver/src/adw/production.rs:50-77`), so `omp adw run` must open one. |
| Telemetry | `vocab!` enums (`crates/observability/src/semconv.rs:59+`), `omp.tool.place` (`crates/observability/src/attrs.rs:209`). | New `omp.exec.*` attributes. |
| Native SSH | `omp ssh exec <alias> <command>` (`crates/app/src/ssh_cmd.rs:77`, `:137`) on `russh` (`Cargo.toml:207`, `crates/envd/Cargo.toml:91`). | A future SSH backend that needs no new client. |
| Toolchain at a fixed path | `fetch-python.sh <dest>` installs the CPython bundle anywhere (`crates/py/scripts/fetch-python.sh:33-41`). omp-py finds the bundle only through `PYO3_CONFIG_FILE` (`crates/py/build.rs:47-70`). The repo `[env]` entry does not override an existing environment variable (Cargo `#env`). | A remote image can carry the bundle at `/opt/omp-python`. |

**Missing:**
- an execution backend abstraction or router;
- a build-system adapter layer (cleanse checkers pick a command, but nothing turns a command into
  a placeable request);
- an input capture that follows git's tracked/untracked rules;
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
  deterministically, and so is every tracked file, even one that matches an ignore rule. Deleted
  files are absent. Secrets and untracked ignored files are never uploaded by default.
- **R5. Typed failures.** A command that fails is never confused with infrastructure that fails.
  Fallback never reruns a failed command elsewhere, and never reruns a command whose remote run
  the server already accepted.
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
  - Providers are data, with code only for "genuinely distinct wire behavior" (`AGENTS.md:161-162`).
  - `omp-envd` owns the filesystem, process, document and tool authorities (`AGENTS.md:41-44`).
    `omp-driver` composes "concrete Tower services, `Inference` implementations, environment
    sessions, and higher-layer host bridges" (`AGENTS.md:186-188`).
    The inference rule "No vendor server-side tools (lock-in)" (`AGENTS.md:513`) is about
    inference providers; applying the same lock-in caution to build backends is an analogy [I].
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
  - 0028 governs network egress of sandboxed commands through its broker and approvals. It says
    nothing about uploads to a build service. Treating such an upload as an egress effect that
    needs the same operator consent is this record's inference [I].
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
- Digest functions SHA256, SHA384, SHA512, SHA1 and BLAKE3 (BB
  `server/remote_cache/digest/digest.go:49-55`, returned by
  `server/remote_cache/capabilities_server/capabilities_server.go:71`). The legacy single
  execution digest is SHA256 (`capabilities_server.go:94`).
- `max_batch_total_size_bytes = 0` ("protocol limit", `:83`).
- ZSTD only when transcoding is enabled (`:65-68`).
- Split/splice support (`:87-88`).
- **User roles and API-key permissions are different things.**
  - User roles (BB `docs/guide-auth.md:117-136`): Admin and Writer read and write CAS and AC;
    Developer reads and writes CAS but only reads AC; Reader only reads.
  - Org API keys carry permission checkboxes instead: "Read-only key (disable remote cache
    uploads)" and "Executor key (for self-hosted executors)" (`:60-66`).
  - A personal (user-owned) key is capped by its owner's role (`:77-83`).
- **No HTTP or WebDAV cache endpoint is documented** in BB `docs/`, so sccache cannot target it
  directly. The absence is UNVERIFIED beyond the docs search. BuildBuddy does implement the Remote
  Asset API (BB `server/remote_asset/fetch_server/fetch_server.go`, `FetchBlob` at `:233`) and
  documents certificate-based auth as an alternative to API keys (BB `docs/rbe-setup.md:178`).

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
  `cli/remotebazel/remotebazel.go:1069`, `:1366-1380`). `Cancel` **skips** any execution that
  another request has been merged onto (`execution_server.go:2113-2121`), so a merged action keeps
  running.
- **`bb execute` defaults to no action cache.** `--cache_read` and `--cache_write` default to
  false, which sends `skip_cache_lookup=true` and `Action.do_not_cache=true` (BB
  `cli/execute/execute.go:52-53`, `:159`, `:200`). It mints a UUID `tool_invocation_id` when none
  is given (`:125-128`). Its API key travels as `--remote_header=x-buildbuddy-api-key=…`, that is
  on argv (`:42`, `:119-123`).
- **Live output.** No reference to REAPI `stdout_stream_name` (REAPI `:1744`) was found in the BB
  sources sampled. Live output for raw REAPI is UNVERIFIED, so assume logs arrive at completion.

**3. Bazel-specific.**
- `--remote_*` flags and `.bazelrc`.
- BES, Bazel's Build Event Protocol. BuildBuddy's ingestion of it and its invocation UI are
  BuildBuddy's own (executions are listed per invocation, BB
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
    Larger sizes are set with `EstimatedCPU`, `EstimatedMemory` and `EstimatedFreeDiskBytes` (BB
    `docs/rbe-platforms.md:283`, `:287`, `:294`; usage example at `docs/remote-bazel.md:88-92`).
  - The runner's default user is the non-root `buildbuddy` (BB `docs/remote-bazel.md:363-366`).
    The documented way to add packages is `sudo apt-get install` in a step (BB
    `docs/workflows-config.md:450-456`).
- **Snapshots cost cache transfer.** Snapshots "include the entirety of the disk and memory of
  the machine": 16 GB RAM and a 50 GB disk give a 66 GB snapshot. They are stored in the remote
  cache, and "cache uploads and downloads are billed" (BB
  `docs/remote-runner-introduction.md:236-243`; also `:446`). They are evicted like any other
  cache entry (BB `docs/remote-bazel.md:496-500`).
  - The save policy is the `remote-snapshot-save-policy` property, settable with
    `--runner_exec_properties` (BB `docs/remote-runner-introduction.md:224-227`, `:244-276`).
  - The default, `first-non-default-ref`, saves a snapshot on **every** run on the default ref,
    and only on the first run of any other ref. Later runs on that ref resume from that first
    snapshot (`:251-262`). Warm state on a feature branch therefore freezes at its first run.
  - A detached or unpushed HEAD falls back to the default branch (BB
    `cli/remotebazel/remotebazel.go:427-441`), so every such run saves a snapshot.
  - Whether a resume on the same executor host downloads anything, and how a resume is metered,
    are UNVERIFIED.
- **Credentials inside the runner (read from source, not run).** The hosted runner receives
  `BUILDBUDDY_API_KEY=<key>`, `REPO_USER` and `REPO_TOKEN=<GitHub App token>` as environment
  overrides (BB `enterprise/server/hostedrunner/hostedrunner.go:400-406`). The CI runner starts
  every step through `runBashCommand` (BB `enterprise/server/cmd/ci_runner/main.go:1238`), and
  `runCommand` passes `os.Environ()` on (`:2831-2836`). The only variables it unsets are
  `CI_RUNNER_ROOT` and `BAZEL_BIN` (`:3039-3042`). It also writes the key into
  `{rootDir}/buildbuddy.bazelrc`, the parent of `repo-root/` (`:76`, `:255-266`, `:2731-2733`).
  So every build script, proc-macro and test the script runs can read the key and the GitHub
  token, and Firecracker networking is `external`. The GitHub App token's scope is UNVERIFIED.

**5. Self-hosted executors ("Bring your own runners").**
- Available on every tier (BB `website/src/pages/pricing.tsx:196`). They need an executor key (BB
  `docs/guide-auth.md:64-66`).
- Per the docs they are the only way to get `darwin` or `windows` (`docs/rbe-platforms.md:239`),
  and `host`/`docker`/`podman`/`sandbox`/`none` isolation (`:255-257`, `:361-372`). Managed Mac
  executors on a paid tier are the exception named in Problem 3 (UNVERIFIED terms).
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
  - Team: managed Mac cores at $45/core; Enterprise: unlimited (`:166-169`). The Team overage rate
    is a literal placeholder `$X` (`:59`). The live page `https://www.buildbuddy.io/pricing`,
    fetched 2026-10-08, also shows a "Mac cores" row with "$45 / core" and "$X / GB"; its text
    rendering does not show which column is empty, so the tier assignment rests on the source.

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
  - The only indirect chain is sccache → a local bazel-remote → its *experimental* gRPC proxy,
    which needs the backend's Remote Asset API for HTTP clients and authenticates by mTLS
    (BR `:251-266`). BuildBuddy has both pieces: a Remote Asset fetch server and certificate auth
    (Findings 1). So the chain may be assemblable, but nobody supports it end to end, and the proxy
    is experimental.
  - Even working, macOS-target outputs can only be shared between Macs, and crates that invoke
    the linker (`bin`, `dylib`, `cdylib`, `proc-macro`) are never cached (SCC `docs/Rust.md:11`).
    Links are the RAM-heavy part. That, not a missing hop, is why this route is rejected.
- **b. One coarse REAPI Action per verification command** (`just test-pkg <crate>`, clippy, a Linux
  e2e proof) over the captured working tree, in a pinned Linux image. **Chosen.**
- **c. One REAPI Action per rustc call via `RUSTC_WRAPPER`.** **Research only.**
  - Cargo still runs build scripts and links locally.
  - Proc-macros must be built for the remote host, so a macOS host with Linux workers breaks.
    Fuchsia forces local execution for exactly this case:
    `https://fuchsia.googlesource.com/fuchsia/+/main/build/rbe/rustc_remote_wrapper.py`, lines
    296-319 as fetched on 2026-10-08. The link is the unpinned `main` branch, so the line numbers
    may drift.
  - Absolute paths defeat cross-worktree keys (`docs/audits/sccache-mac-measurement.md:95`).
  - The only Cargo client, `cargo-reapi`, is experimental and has not been validated against a
    production REAPI service (`https://github.com/TamedTornado/cargo-reapi`).

Near-term relief, ranked:

| # | Route | What leaves the Mac | New omp code | Friction | Lock-in | Verdict |
|---|---|---|---|---|---|---|
| R0 | Stop this laptop serving omp CI (`vars.MACOS_RUNNER`, runner service) | Up to all CI load (879 min of Rust jobs, 979 min of all omp jobs over 3 days, above) | 0 | The hosted macOS job "take[s] over an hour" (`docs/parked/2026-10-06-handoff-pending-work.md:160-161`). Alternatively, split Linux-valid tests to hosted Linux. | none | Owner decision outside this ADR, probably the single largest item |
| R1 | `bb remote --script` operator trial | Linux test, clippy and Linux e2e: compile, link and run | 0 | Account in a dedicated org; a key with cache writes; link the private repo; untracked `MODULE.bazel` marker; a pushed non-default branch; VM sizing against snapshot transfer; first-run toolchain install; key and GitHub token readable by every process in the runner; the Linux suite was never gated | High (Run API), but nothing in the repo depends on it | **Slice 0** |
| R2 | omp coarse REAPI Action (`omp-exec` + `omp-exec-reapi`) | Same as R1 | 2 crates, an envd capture entry point, driver/app wiring | Image build and publish; credential; warm-state tuning | Functional: low (any REAPI server runs the action). Performance: high, because warm state depends on BuildBuddy's runner recycling; without it every action is a cold build (405-656 s on the 4-vCPU Linux reference) | **Slice 1** |
| R3 | `bb execute` per command | Same, cold unless the runner is recycled | 0 (script) | Undocumented; needs a staged clean input dir; the key goes on argv (`--remote_header`) | High | **Slice 0b** as a probe of the raw-REAPI path only, not as the way to work |
| R4 | SSH to a Linux box or a second Mac (`omp ssh exec`) | Same; a Mac box can also take macOS proofs | 0 now, a backend later | Needs a machine | none | **Slice 3** backend; usable today if a box exists |
| R4b | Managed Mac executors on a BuildBuddy paid tier | macOS proofs, macOS `cfg` coverage, possibly the dev binary's verification | Same as R2 with a `darwin` profile | Team tier at $45/core per the pricing source; terms through sales; UNVERIFIED | High | Ask BuildBuddy before deciding; not in any slice |
| R5 | sccache plus a remote cache | Hits only | 0 | No BuildBuddy route (a) | n/a | Rejected for BuildBuddy. Local sccache is a separate, measured-mixed decision. |
| R6 | Per-rustc `RUSTC_WRAPPER` | rlib codegen only | Large | (c) | medium | Research |
| R7 | Bazel `rules_rust` | Everything, per action | Very large | Migration | low | Rejected (A) |

## Decision

### Part A — Shape

1. **Layers.** A request flows through these layers in order. A backend MUST NOT inspect argv to
   infer a build system. An adapter MUST NOT name a backend.

   ```
   caller: ADW code phase | cleanse checker | (later) agent job
      │  BuildCommand { argv, cwd, intent, requirement: omp_adw::Requirement }
      ▼
   BuildSystemAdapter: CargoJust | Maven | Gradle | Go | Dotnet | Npm | Pnpm | Python | Bazel
      │  argv, env allowlist, output paths, toolchain, named caches, needs, eligibility
      ▼
   ExecutionPlan (IR): ExecutionRequest[] with dependencies (one request in slice 1)
      ▼
   ExecutionRouter: pure placement over backend capabilities × policy × eligibility × Requirement
      ▼
   ExecutionBackend: Local (envd ExecHost) | Reapi (profile: generic | buildbuddy) | Ssh | …
      ▼
   ExecutionResult → journal entry + CAS refs inside a session; a CLI report for ADW today
   ```

   The adapter trait and the plan live in `omp-exec` (Part B). Slice 1 ships one adapter,
   `CargoJust`; the others are added later without touching a backend.

2. **Unit of remote work.** It is a coarse Action: one whole verification command over a captured
   working tree. Per-rustc remote execution is not on the roadmap until slice 1 measurements and a
   proven Cargo REAPI client exist.
3. **Backends implement one trait.** Remote backends speak REAPI v2.
   - **BuildBuddy is mostly a profile, as data:** the auth header name, the platform property
     names (`OSFamily`/`Arch` against REAPI's standard `OSFamily`/`ISA`, REAPI `platform.md:11-30`),
     and the isolation and recycling properties.
   - **One piece is code:** invocation cancel calls the proprietary
     `BuildBuddyService.CancelExecutions` RPC, which needs vendored BuildBuddy protos and client
     code. It is a capability-gated module in `omp-exec-reapi` (feature `buildbuddy-cancel`),
     which `AGENTS.md:161-162` permits as "genuinely distinct wire behavior". Its proto licence is
     part of the licence review.
   - No other code branches on a vendor name. This is the same rule as providers-as-data
     (`AGENTS.md:507`).
   - **Where the code lives.** File and blob reads (capture, `BlobSource`) stay in `omp-envd`,
     which owns those authorities, and are reached through the env protocol. The REAPI client is a
     network service composed by `omp-driver`, the same way driver composes the inference Tower
     services that already send project content to third parties (`AGENTS.md:186-188`). An SSH
     backend lives in `omp-envd`, which owns `russh` and remote process control (ADR 0006: "point
     the stub at … a remote machine"). Agent-initiated remote runs (a later slice) must pass the
     same operator approval as other egress, keyed by the endpoint host.
4. **Bazel products** are planned by a Bazel adapter as one Local-placed request (`bazel test …`).
   Their remote offload is Bazel's own configuration. omp records the result and never wraps
   Bazel's actions.

### Part B — Domain model (crate `omp-exec`: pure, no I/O, no vendor names)

```rust
omp_core::string_id!(ExecutionId);        // a UUID minted per run; also the REAPI tool_invocation_id
omp_core::string_id!(BackendId);          // "local", "remote", "ssh:<alias>" — operator-named
omp_core::string_id!(ToolchainId);        // adapter-derived from repo pins, e.g. "rust:nightly-2026-08-08+nextest-0.9.146+cpython-20260807"
omp_core::string_id!(CacheName);          // e.g. "cargo-target"

/// What a caller asks for, before any build-system knowledge is applied.
pub struct BuildCommand {
   pub argv: Arc<[Str]>, pub cwd: Option<Str>, pub intent: Intent,
   pub requirement: omp_adw::Requirement, // the phase's ceiling (crates/adw/src/profile.rs:105-112)
}
/// Pure, no I/O: repo pins (rust-toolchain.toml, nextest pin, fetch-python TAG/VER) arrive as data.
pub trait BuildSystemAdapter {
   fn plan(&self, command: &BuildCommand, pins: &RepoPins) -> Result<ExecutionPlan, PlanError>;
}
pub struct ExecutionPlan { pub requests: Arc<[ExecutionRequest]>, pub edges: Arc<[(u16, u16)]> }

pub struct ExecutionRequest {             // cold, moved by value, cloned into telemetry → Arc-backed fields
   pub id:          ExecutionId,
   pub intent:      Intent,               // strum: Check | Lint | Test | Build | Proof | Format | Other
   pub argv:        Arc<[Str]>,           // argv[0] a bare name resolved on the action PATH; never a shell string
   pub cwd:         Str,                  // relative to the input root; the root itself is "" (never ".")
   pub env:         Arc<[EnvEntry]>,      // adapter allowlist; the profile adds the image env (Part G)
   pub inputs:      InputSet,
   pub outputs:     OutputSpec,
   pub toolchain:   ToolchainId,          // the backend profile maps it to an image + image env
   pub caches:      Arc<[CacheDecl]>,     // CacheDecl { name: CacheName, env: Str } e.g. cargo-target → CARGO_TARGET_DIR
   pub platform:    PlatformRequirement,
   pub requirement: omp_adw::Requirement, // checked against each candidate backend's enforced posture
   pub resources:   ResourceRequirements,
   pub eligibility: Eligibility,          // adapter-derived, see Part C
   pub timeout:     Duration,             // always set; becomes Action.timeout
}
pub struct InputSet { pub manifest: BlobRef, pub files: u64, pub bytes: u64, pub excluded: ExclusionCounts }
pub enum OutputSpec { Metadata, Logs, Paths(Arc<[Str]>), All }
pub struct PlatformRequirement {
   pub os: Os, pub arch: Arch,            // strum enums: Linux|Macos|Windows, X86_64|Aarch64
   pub needs_network: bool,               // what the command needs (cargo fetching crates), not what it tolerates
   pub needs: NeedSet,                    // SparseSet<Need>: Pty, Seatbelt, Docker, MacosSdk, Gpu, WorkspaceWrite, HostTiming
}
pub struct ResourceRequirements { pub cpu_millis: u32, pub memory_bytes: u64, pub disk_bytes: u64 }
pub struct BackendCapabilities {
   pub platforms: Arc<[(Os, Arch)]>, pub needs: NeedSet, pub max_timeout: Option<Duration>,
   pub action_cache: CacheSupport, pub cancel: CancelSupport, pub max_blob_bytes: Option<u64>,
   pub warm_caches: bool,                 // can it keep named caches between runs?
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

/// Mirrors omp_tools::shell::ShellExec (crates/tools/src/shell.rs:345-372): Clone + 'static, owned Run.
pub trait ExecutionBackend: Clone + Send + Sync + 'static {
   type Run: ExecutionRun;                               // impl-inferred, unboxed (AGENTS async rules)
   fn id(&self) -> &Id<str>;
   fn capabilities(&self) -> &BackendCapabilities;
   fn start(&self, request: ExecutionRequest)
      -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_;
   fn reattach(&self, handle: Reattach)                 // after a disconnect or a restart (WaitExecution)
      -> impl Future<Output = Result<Self::Run, ExecutionError>> + Send + '_;
}
/// Mirrors ShellRun (crates/tools/src/shell.rs:332-342). One cancel path: `cancel`.
/// Dropping a run that was not detached cancels it (RAII, like envd's ExecRun).
pub trait ExecutionRun: Send + 'static {
   fn next_event(&mut self) -> impl Future<Output = Option<ExecutionEvent>> + Send + '_;
   fn cancel(&self) -> impl Future<Output = CancelOutcome> + Send + '_;
   fn detach(self) -> Reattach;                        // Reattach { backend: BackendId, operation: Str, id: ExecutionId }
}
pub fn place(request: &ExecutionRequest, backends: &[&BackendCapabilities], policy: &ExecutionPolicy)
   -> Result<Placement, Unplaceable>;                  // pure; proptested from slice 2
```

- **Dispatch.** The driver holds `enum Backend { Local(LocalBackend), Reapi(ReapiBackend),
  Ssh(SshBackend) }` and matches on it, per AGENTS "enums before `dyn`". The trait is never used
  as `dyn`.
- **Cancellation.** A caller's `CancellationToken` is bridged to `ExecutionRun::cancel` by the
  caller; the backend sees one cancel path.
- **Digests.** `BlobRef` is the digest. If `omp-exec` cannot take an `omp-journal` dependency,
  `BlobRef` *moves* into `omp-core` in the same change (rename+move, never a copy).
- **Events.** `ExecutionEvent` carries `Queued`, `Uploading{done, total}`, `Accepted{operation}`,
  `Executing`, `Output(OutputFrame)` (local only), and
  `Settled(Result<ExecutionResult, ExecutionError>)`. `Accepted` marks the point after which
  fallback is forbidden (Part C).
- **REAPI mapping.** `cwd` "" maps to an empty `Command.working_directory`; BuildBuddy rejects an
  absolute path, a non-local path and `"."` (BB `execution_server.go:834-837`).

### Part C — Placement, eligibility, fallback, failures

1. **Eligibility is derived by the adapter from the command and the repo, and matched against the
   backend's capabilities. The caller never declares it.**
   - The `CargoJust` adapter carries a checked-in needs table for this repo's recipes and proofs,
     as data in `omp-exec` (for example `test-pkg`, `check-pkg`, `clippy`, `e2e-core`: none;
     `e2e-p7`: `Pty`; `e2e-p12`: `Seatbelt`; `e2e-p8` and `e2e-baseline`: `HostTiming`; `fmt`:
     `WorkspaceWrite`; recipes at `justfile:26-241`).
     A command it does not know is `LocalPreferred`.
   - A project file can only *narrow* placement. The workflow schema gains one optional per-phase
     field, `placement = "local"`, next to the existing `requires` (`FilePhase` is
     `deny_unknown_fields`, `crates/driver/src/adw/definition.rs:223-247`). No project field can
     send a phase remote; only the operator's policy can.
   - The same crate yields *different* requests per target OS.

   | Class | When |
   |---|---|
   | `LocalRequired` | Needs `Seatbelt`, `MacosSdk`, or `Pty` when no backend offers one. Builds this host's developer binary. `HostTiming`: P8-style timing recordings (`.github/workflows/p8-baseline.yml:50-52`). Release packaging. TUI real-PTY QA (ADR 0033). `WorkspaceWrite`: commands that change files (`just fmt`, `cargo fix`, codegen), because a remote action cannot write the local workspace and would exit 0 having changed nothing; this holds until declared outputs can be applied back through the document authority (not before slice 2). |
   | `RemoteRequired` | The requested platform does not exist locally: Linux verification on this Mac, including landlock/bubblewrap paths and the Linux `cfg` code that no local run exercises. Under **every** policy it is never placed locally: with no admitting remote backend it is `Unplaceable`. |
   | `RemotePreferred` | A Linux-valid build, test or lint with no PTY whose inputs are the working tree plus the image: `just test-pkg <crate>`, clippy on Linux, P1-P5, P9-P11, `tool_sources`. |
   | `LocalPreferred` | Short commands where upload and queueing dominate. Commands the adapter does not know. Requests whose input closure is not fully captured. |

2. **Admission is per placement.** `place()` admits a backend only when its `enforced` posture
   satisfies the request's `requirement` (`Requirement::check`, `crates/adw/src/profile.rs:231`),
   and checks again for any fallback placement. A request with `needs_network` is never placed
   on a backend whose enforced network is `Disabled`. Today ADW checks one host posture once per
   run, before the first dispatch (`crates/driver/src/adw/mod.rs:84-91`, `:132-135`), and that
   posture describes the shell-session sandbox (`crates/driver/src/adw/production.rs:152-173`)
   while code phases run unconfined (`:102-145`, F11). So `AdwHost` changes to report posture per
   dispatch and per placement.

3. **Fallback policy** (`sv_exec_policy`). Slice 1 has no fallback and no router: under
   `--exec-policy remote-only` (or the convar), phases the adapter classes `RemotePreferred` or
   `RemoteRequired` go to the REAPI backend, every other phase keeps today's local path, and a
   remote failure is reported. The table applies from slice 2, when a Local backend exists.
   "Accepted" means the server returned an operation name (`ExecutionEvent::Accepted`).

   | Policy | Placement | Remote failure before acceptance | Remote failure after acceptance | Exited ≠ 0 | Cancelled |
   |---|---|---|---|---|---|
   | `local-only` (default) | local; `RemoteRequired` → `Unplaceable` | n/a | n/a | report | stop |
   | `local-preferred` | local; `RemoteRequired` → remote | fail, typed | reattach, else cancel and fail | report | stop |
   | `remote-preferred` | remote for `RemotePreferred` and `RemoteRequired`; local for the rest | run local only if `sv_exec_fallback_local` is on and the request is not `RemoteRequired`; record `fallback`. Otherwise fail, typed | reattach with `WaitExecution`; if that fails, cancel and fail, typed. **Never rerun** | **report; never rerun** | stop |
   | `remote-only` | remote; `LocalRequired` → `Unplaceable` | fail, typed | reattach, else cancel and fail | report | stop |

   Local fallback of a heavy command is opt-in (`sv_exec_fallback_local`, default off), because an
   automatic local rerun of `just test-pkg` on a swapping Mac brings back the load this record
   removes.

4. **Failures are typed** (`thiserror`). Facts are named fields and causes are `#[source]`.
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
   - **Fallback-eligible, and only before acceptance:** `Transport`, `Capacity`, `Infrastructure`,
     upload-side `Cas`, and `Auth` (with a loud notice).
   - **Never fallback-eligible:** every failure after acceptance; `TimedOut`, `Cancelled`,
     `InputRefused`, `InputCapture`, `Config` (a misconfiguration is reported, never masked),
     `OutputRetrieval` and download-side `Cas` (the command already ran), `Unplaceable` for a
     `RemoteRequired` request, and `Exited`.
   - The classification is one exhaustive `match` on `(ExecutionError, Stage)` with no wildcard
     arm, so a new variant does not compile until it is classified. A table test pins it from
     slice 1 and the placement proptest from slice 2.
   - **How ADW sees results.** An `ExecutionError` becomes a `ProductionError`, which aborts the
     run as `Error::Host` (`crates/driver/src/adw/mod.rs:116`, `:160-163`). Only
     `Outcome::Exited` with a non-zero code becomes a rejection, and its feedback is the bounded
     log projection. Today a cancel is reported as a rejection with feedback "cancelled"
     (`production.rs:123-124`), and rejection feedback is the next attempt's correction
     (`mod.rs:27-31`, `:163-167`); a cancel must stop the run instead.

### Part D — Working-tree input capture

1. **The input set follows git, not the walker.** It is:
   - every path in the index (`omp-vcs` `ls_files(false, _)`, `crates/vcs/src/lib.rs:224`),
     **including force-added files that match an ignore rule**, at its *working-tree* content, so
     staged and unstaged edits are both what the developer sees;
   - plus untracked paths that git does not ignore (`ls_files(true, true)`, the same as
     `git ls-files --others --exclude-standard`);
   - minus index paths that are missing from the working tree (deleted files are absent);
   - never `.git`, and never paths git ignores unless they are tracked (`target/`, `vendor/` per
     `.gitignore:2`, `:7`). `.ignore` files have no effect, because git does not read them.

   Why not envd's snapshot walk as is: it drops tracked files that match an ignore rule.
   `git ls-files -ci --exclude-standard` lists five such files today. Two are force-added test
   fixtures (`crates/tools/tests/fixtures/special-sources/tree/ignored.txt` and
   `tree/nested/ignored-generated.txt`, ignored by that tree's `.gitignore:6-7`), and
   `special_source_fixture_workspace_is_complete_and_self_contained`
   (`crates/tools/tests/it/read.rs:648-675`) asserts that they exist. A remote
   `just test-pkg omp-tools` over the walker's set would fail where the local run passes. ADR 0007
   records the same gap for isolated worktrees (`docs/adr/0007-subagent-filesystem-isolation.md:81`).

   **Where capture runs.** In `omp-envd`, as a capture-only entry point next to `snapshot_at`
   (`operations.rs:762`). It takes the path list above, applies the deny rules (D.2) *before*
   hashing, hashes each kept file with `hash_file` into the blob store and writes the manifest. It
   takes no `transition` lock and publishes no generation or snapshot record, unlike the public
   `snapshot()` (`:384-396`, `:866-905`), so a remote run neither adds a restore point nor
   conflicts with agent workspace transitions. Index-only ("staged") capture is a later option,
   not the default.
2. **Secret exclusions.** One behaviour per case, so the counts and the errors agree:
   - **An untracked file that matches** a deny rule is **excluded** (never uploaded) and counted
     per rule in `ExclusionCounts`.
   - **A tracked file that matches** **refuses** the run with `InputRefused{path, rule}`, because
     excluding it would make the remote tree silently differ from the repository. No tracked file
     matches today (checked with `git ls-files`).
   - The operator can allow a path with `sv_exec_upload_allow` (user scope only, like every
     `sv_exec_*` setting).
   - These defaults are needed because omp2's `.gitignore` has no `.env` entry. Name patterns:
     - `.env`, `.env.*` and `.envrc`
     - `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa*`, `id_ed25519*`
     - `.ssh/**`, `.aws/**`, `.gnupg/**`, `.kube/**`
     - `.netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, `.docker/config.json`, `*.kdbx`
     - `*.tfvars`, `*.tfstate`
   - **Content scan.** Untracked files and tracked files that differ from `HEAD` are also scanned
     with `omp_secrets::credential_rules()` (`crates/secrets/src/builtins.rs:18`) before upload. A
     hit is treated like a name match: excluded if untracked, refused if tracked.
   - Untracked files follow `sv_exec_upload_untracked` (`non-ignored` by default, or `none`).
3. **Determinism.**
   - Paths are sorted.
   - Mode is normalized to REAPI `is_executable`.
   - No mtimes or `node_properties`.
   - **Symlinks.** A tracked symlink refuses the run in slice 1 (the tree tracks none today). An
     untracked symlink is skipped and counted. Every `omp2-wt-*` worktree but one has an untracked,
     unignored `vendor` symlink to the main checkout's `vendor/`, because `.gitignore:7`
     (`/vendor/`) matches only directories. Changing that line to `/vendor` would ignore it; that
     is a separate one-line change.
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
  - `All`: needs an explicit operator setting, because a test binary of a crate downstream of
    `omp-py` is ~650 MB.
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
| `sv_exec_policy` | `local-only` | Placement and fallback policy (Part C) |
| `sv_exec_fallback_local` | `0` | Whether a remote-placed request may fall back to local before acceptance (slice 2) |
| `sv_exec_backend` | `local` | Name of the preferred backend |
| `sv_exec_remote_profile` | `reapi` | Data profile: `reapi` or `buildbuddy` |
| `sv_exec_remote_endpoint` | empty | `grpcs://host[:port]`; empty disables remote. A validate hook refuses `grpc://` and any plaintext scheme except to a loopback address (the fake-server tests), so the key header is never sent in the clear. |
| `sv_exec_remote_instance` | empty | REAPI `instance_name`, opaque (REAPI `:225`) |
| `sv_exec_remote_credential` | empty | A credential *source*: a stored entry or a `!command` (for example `!security find-generic-password -s buildbuddy -w`), resolved through `CommandCredentials` (10 s, 1 MiB cap), which needs an envd environment (Context). Never the value. |
| `sv_exec_remote_tls_ca` | empty | Optional CA bundle; mTLS cert/key are later |
| `sv_exec_remote_images` | empty | A `Kv` from `ToolchainId` to a digest-pinned image reference; the profile also supplies that image's env (Part G) |
| `sv_exec_remote_platform` | empty | Extra properties as a `Kv`, mapped by the profile |
| `sv_exec_remote_timeout_ms` | `1800000` | `Action.timeout`. 30 min, above the 21 min p90 of the whole macOS CI job, and the bound on leaked remote work when cancel fails. |
| `sv_exec_remote_concurrency` | `1` | In-flight remote actions |
| `sv_exec_remote_cache` | `off` | Action-cache use. `off` sends `skip_cache_lookup=true` and `Action.do_not_cache=true` (REAPI `remote_execution.proto:663-668`, `:717-719`, `:1596-1605`), which also stops in-flight merging. `read` and `read-write` are refused while the profile uses runner recycling or named warm caches, and an AC hit is advisory only (Part G). |
| `sv_exec_upload_exclude` | empty | Extra exclusion globs, added to the defaults |
| `sv_exec_upload_allow` | empty | Paths the operator allows despite a deny rule |
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
  `darwin` (a self-hosted executor, a managed Mac on a paid BuildBuddy tier, or a second Mac over
  SSH).
- **Who owns what.**
  - The **adapter** derives a `ToolchainId` from the repo pins (`rust-toolchain.toml`, the nextest
    pin, the fetch script's `TAG`/`VER`) and declares named persistent caches with the environment
    variable that selects them (`cargo-target` → `CARGO_TARGET_DIR`). It never names an image.
  - The **backend profile** maps a `ToolchainId` to an image plus that image's environment
    (`sv_exec_remote_images`), and maps each named cache to a stable path where the backend can
    keep warm state. A Maven, Go or npm adapter therefore needs a new image entry, not a backend
    change. Local and SSH backends map a toolchain to "already installed here" and caches to a
    local directory.
  - The **action env** is the adapter's allowlist plus the profile's image env, which pins `PATH`,
    `HOME`, `CARGO_HOME`, `RUSTUP_HOME` and `PYO3_CONFIG_FILE` to image paths. With no `PATH` in the
    action env, REAPI leaves argv[0] resolution "implementation-defined" (REAPI
    `remote_execution.proto:765-771`).
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
  - The command copies the input root into a fixed in-VM path with
    `rsync -rlp --checksum --delete --exclude=/target/ --exclude=/vendor/` and builds there, with
    the `cargo-target` cache outside the workspace.
    - `--delete` removes files deleted locally, so R4 holds: cargo auto-discovers `tests/*.rs` and
      `examples/`, and a stale copy would still compile and run. Excluded paths are protected from
      deletion.
    - No `-t` (which `-a` implies): rsync would copy the executor's input mtimes onto unchanged
      files, and build scripts still use mtimes even under `-Zchecksum-freshness`, which is on
      (`.cargo/config.toml:53`; Cargo `#checksum-freshness`: "files ingested by build script will
      continue to use mtimes"). Unchanged files keep their old mtimes; changed files get the
      current time.
    - Fixed paths keep cargo fingerprints stable across runs [I].
  - Network is `external` under Firecracker. `Cargo.lock` checksums pin every registry crate.
  - **Warm state and the action cache never mix.** A recycled, networked runner with state outside
    the inputs is not hermetic (BuildBuddy: recycling "reduces action hermeticity", BB
    `docs/rbe-platforms.md:258-263`). So a profile that uses recycling or named warm caches always
    sends `skip_cache_lookup=true` and `do_not_cache=true`. An AC hit is never an ADW or cleanse
    acceptance until a signed or salted verdict scheme exists, because anyone holding a key with AC
    write can write entries.
  - **This warm path is BuildBuddy-specific.** `recycle-runner`, `runner-recycling-key`,
    Firecracker and the `Estimated*` sizes are BuildBuddy properties (BB
    `docs/rbe-platforms.md:255-292`). On a generic REAPI server without recycling, every coarse
    action is a cold build: 405-656 s on the 4-vCPU Linux reference
    (`docs/audits/cranelift-panic-cleanup.md:486-508`). Generic alternatives that keep warm state:
    the SSH backend's persistent cache directory, or sccache inside the image pointed at S3 or GCS
    (SCC `README.md:35-47`).
- **A hermeticity gap to close.** `fetch-python.sh` does not verify the upstream `SHA256SUMS` of the
  CPython archive (`fetch-python.sh:53`, `:70`).

### Part H — Security model

1. **The remote CAS and the executor are a trust boundary.** Everything uploaded is disclosed to
   the provider. Enabling a remote backend is an operator act through user cfg. Every result
   records the backend id and endpoint host.
2. **Defaults block the usual leaks.**
   - Secret-pattern exclusions and the content scan (Part D).
   - An action env built from an allowlist, never by inheriting the host environment. ADW code
     phases inherit everything today (F11).
   - No credentials in actions. omp's own REAPI path sends the key only as gRPC metadata, over TLS
     (Part F).
   - **Slice 0 breaks this rule, and the operator must accept it knowingly.** `bb remote` puts the
     API key and a GitHub App token into the environment of every process the script starts, and
     the key into a bazelrc beside the checkout (Findings 4). Every build script, proc-macro and
     test from the repo and its 1315 registry packages can read them, with `external` network.
     A key with AC write could then forge cached results. Mitigations for Slice 0:
     - use a dedicated BuildBuddy organization with its own key, and delete the key after the trial;
     - never let that organization's action cache gate an omp verdict;
     - start the script with `unset BUILDBUDDY_API_KEY REPO_TOKEN REPO_USER` (this cannot hide the
       bazelrc copy);
     - treat the GitHub App token's scope as UNVERIFIED until BuildBuddy's GitHub App permissions
       are read.
3. **Logs and outputs that come back are untrusted data**, like any tool output. They pass the ADR
   0009 bound and the `omp-secrets` redaction at presentation. Downloaded outputs are never
   executed locally unless a caller asks.
4. **Admission is by enforced posture, per placement** (`Requirement::check`,
   `crates/adw/src/profile.rs:231`; Part C.2).
   - A remote Linux action cannot write the local workspace at all, so the local
     `WriteScope` is `ReadOnly`. That is also why `WorkspaceWrite` phases are `LocalRequired`.
   - Its network scope is the one its platform enforces (`off` → `Disabled`, `external` →
     `Unrestricted`).
   - Each placement, including a fallback, is checked against the phase's `Requirement`.
   - Agent-initiated remote runs (a later slice) ask the operator through the approval path used
     for network egress, keyed by the endpoint host [I] (see Constraints, ADR 0028).
5. **Repo-planted binaries never resolve remotely.** argv[0] resolves on the executor PATH inside
   the image. Locally, `FilesystemResolver`'s project-first lookup
   (`crates/driver/src/cleanse/checkers.rs:30-60`) keeps applying only to local runs.

### Part I — Cancellation

| omp event | Local backend (slice 2) | REAPI (generic profile) | BuildBuddy profile (slice 1) |
|---|---|---|---|
| Token fires: turn interrupt, job cancel, or `omp adw run` Ctrl-C (wired in slice 1) | TERM to pgids, 250 ms grace, KILL (`exec.rs:84`, `:2416-2426`) | Stop waiting. Call LRO `CancelOperation` (best-effort, LRO `operations.proto:88-101`). REAPI capabilities have no cancel field (`ServerCapabilities` at REAPI `:2206`), so an `UNIMPLEMENTED` answer is how the client learns it is unsupported. Settle `Cancelled{remote: Requested\|Unsupported}`. | `CancelExecutions(invocation_id)` as `bb remote` does, through the feature-gated module (Part A.3). It skips executions other requests merged onto (`execution_server.go:2113-2121`): settle `Cancelled{remote: Requested}` and report that a merged action may still run until `Action.timeout`. Dropping the stream alone leaks the action until `Action.timeout`. |
| Timeout | Local timeout | `Action.timeout`, always set (REAPI `:715`) | Same, plus `termination-grace-period` |
| Disconnect, sleep or crash | n/a | Reattach with `WaitExecution(name)` (REAPI `:136`) | Same |

- **Each `ExecutionId` is a UUID and is the REAPI `tool_invocation_id`** in `RequestMetadata`
  (REAPI `:2622`), so a cancel touches only this run. `bb execute` also mints a UUID; whether
  BuildBuddy requires one is UNVERIFIED.
- **Slice 1 local path.** Until the Local backend lands in slice 2, a local code phase stays a
  `kill_on_drop` child with no process group (`crates/driver/src/adw/production.rs:113-124`), so
  `cargo` and `rustc` grandchildren survive a cancel. That is F11, not something this record
  fixes in slice 1.
- **Guarantees.**
  - Cancellation is guaranteed *locally*: results are never applied after cancel.
  - Remotely it is best-effort and bounded by `Action.timeout` (30 min by default).
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
  `.omp/workflows/verify.toml` with `command = ["just", "test-pkg", "omp-envd"]` is the slice 1
  entry point. `omp-envd` links CPython, so it exercises the link-bound path; `omp-core` would not.
- **`omp cleanse`** (`cli.rs:1166-1167`; families at `checkers.rs:164-200`) in slice 2.

Placement comes from the convars, and `omp adw run --exec-policy <p>` overrides them for one
process. Output adds one line per phase, for example
`remote (reapi remote.example:443) 4m12s: queue 3s, upload 1.2 MB / 41 blobs, exit 0`.
For `omp adw run` to honor cfg at all, it must use `process_ctx_with`.

### Part L — Recommended slices

- **Slice 0: operator trial with `bb remote`, no product code, this week. The recommended first
  slice.** It measures relief through BuildBuddy's Run API: a warm Firecracker VM per branch, with
  a BES-backed UI. It does **not** exercise omp's raw-REAPI path (Slice 0b does).
  1. Create a BuildBuddy account with a **dedicated organization for the trial**. Create an org
     API key with "read-only" unchecked, or a personal key of a Writer or Admin user (BB
     `docs/guide-auth.md:60-66`, `:77-83`, `:117-136`); snapshot reuse needs AC write (BB
     `docs/remote-bazel.md:461-468`). Store it in the macOS Keychain and delete it after the trial.
  2. Install `bb`.
  3. Link `blyzer/oh-my-pi` (private) to BuildBuddy. This installs BuildBuddy's GitHub App, whose
     token reaches the runner (Part H.2).
  4. Work from a fresh worktree with no `vendor` symlink (for example
     `git worktree add ../omp2-wt-bb`; no local build happens there), or first change
     `.gitignore:7` to `/vendor`. `bb remote` mirrors untracked, unignored files with
     `git diff --no-index` (BB `cli/remotebazel/remotebazel.go:349-360`, `:547-563`), so the
     existing worktrees' `vendor` symlink would arrive as a dangling link, and
     `fetch-python.sh`'s `mkdir -p` (`fetch-python.sh:37-38`) would fail on it [I, not run].
  5. Create an empty `MODULE.bazel` at that worktree's root, listed in `.git/info/exclude`. It is
     never committed, and git mirroring skips excluded files.
  6. Push a dedicated non-default branch (for example `rbe-trial`) and run from it, so that only
     its first run saves a snapshot. A detached or unpushed HEAD falls back to the default
     branch, where every run saves one (Findings 4). Also set `GIT_REPO_DEFAULT_BRANCH=omp2`. The
     default branch already resolves to `omp2` (`git symbolic-ref refs/remotes/origin/HEAD` gives
     `origin/omp2`; `git ls-remote --symref origin HEAD` gives `refs/heads/omp2`). The variable
     only stops `determineDefaultBranch` from writing `remote-bazel-default-branch` into
     `.git/config` (BB `cli/remotebazel/remotebazel.go:269-293`).
  7. Check that `git ls-files --others --exclude-standard` lists no secrets.
  8. Keep the script outside the repo, for example `~/rbe-trial.sh` [sketch, not run]:
     ```bash
     set -euo pipefail
     unset BUILDBUDDY_API_KEY REPO_TOKEN REPO_USER       # Part H.2; the bazelrc copy remains
     if ! command -v cmake >/dev/null || ! command -v ninja >/dev/null; then
       sudo apt-get update
       sudo apt-get install -y build-essential cmake ninja-build zstd curl git python3 rsync
     fi
     if [ ! -x "$HOME/.cargo/bin/rustup" ]; then          # the runner user is non-root `buildbuddy`
       curl --proto '=https' -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
     fi
     . "$HOME/.cargo/env"                                 # rust-toolchain.toml installs the pinned nightly
     command -v just >/dev/null || cargo install --locked just
     cargo nextest --version 2>/dev/null | grep -q '0\.9\.146' || cargo install --locked cargo-nextest@0.9.146
     [ -f vendor/python/pyo3-config.txt ] || crates/py/scripts/fetch-python.sh
     just test-pkg omp-envd                               # links CPython: the heavy path
     ```
  9. Run it, sized against snapshot transfer. By the docs' arithmetic, 16 GB RAM plus a 40 GB disk
     is a snapshot of about 56 GB:
     ```bash
     BUILDBUDDY_API_KEY="$(security find-generic-password -s buildbuddy -w)" \
     GIT_REPO_DEFAULT_BRANCH=omp2 \
     bb remote --os=linux --arch=amd64 \
       --runner_exec_properties=EstimatedCPU=8 \
       --runner_exec_properties=EstimatedMemory=16GB \
       --runner_exec_properties=EstimatedFreeDiskBytes=40GB \
       --runner_exec_properties=remote-snapshot-save-policy=first-non-default-ref \
       --runner_exec_properties=snapshot-read-policy=local-first \
       --timeout=60m --script="$(<~/rbe-trial.sh)"
     ```
     `--script="$(<file)"` is the documented form (BB `docs/remote-bazel.md:264-269`), and the
     snapshot properties are those of BB `docs/remote-runner-introduction.md:197-227`. Changing any
     size changes the snapshot key, so the next run is cold.
  10. Record the cache transfer of every run from BuildBuddy's usage view. **Stop** when the month's
      total reaches 60 GB of Personal's 100 GB, or when one run moves more than 30 GB.
  11. Measure as in "Measurement plan".

  This gives relief from the first warm run. It answers: VM sizing, snapshot cost, PTY, nested
  sandboxes and pre-existing Linux failures. It cannot answer questions about raw REAPI. It is
  disposable: nothing in the repo depends on `bb`.
- **Slice 0b: raw-REAPI probe, no product code, alongside Slice 0.** It checks the assumptions
  Slice 1 rests on, with the same trial organization and a throwaway key [sketch, not run]:
  1. From the same worktree as Slice 0 (no `vendor` symlink), stage an input root the way Part D.1
     defines it, for example
     `git ls-files -z -co --exclude-standard | xargs -0 ls -d 2>/dev/null | tar -cf - -T - | tar -xf - -C "$STAGE"`
     (deleted paths drop out at `ls -d`).
  2. Submit it with `bb execute` (BB `cli/execute/execute.go:36-80`):
     `bb execute --remote_header=x-buildbuddy-api-key="$KEY" --exec_properties=workload-isolation-type=firecracker --exec_properties=container-image=docker://gcr.io/flame-public/rbe-ubuntu24-04@sha256:<pinned digest> --exec_properties=recycle-runner=true --exec_properties=runner-recycling-key=omp-probe-1 --exec_properties=EstimatedCPU=8 --exec_properties=EstimatedMemory=16GB --exec_properties=EstimatedFreeDiskBytes=40GB --input_root="$STAGE" --remote_timeout=30m --output=json -- bash -c '<install steps>; pwd; stat -c %Y Cargo.toml; just test-pkg omp-envd'`.
     The image is BuildBuddy's pinned Ubuntu 24.04 (BB `server/util/platform/platform.go:38`),
     because the default is 16.04. Whether the action runs as root, which the `apt-get` install
     steps need, is UNVERIFIED. The key sits on argv, visible to local `ps` while it runs, which is
     acceptable only for a throwaway key. Cache reads and writes stay off by default.
  3. Call `GetCapabilities` once, for example with `grpcurl` and the pinned REAPI proto (tooling
     UNVERIFIED). It shows API versions, compressors (so whether zstd is on), digest functions and
     the batch limit. It does **not** show the message-size limit, the maximum timeout, quotas,
     managed arm64 or snapshot accounting.
  4. It answers: whether a raw-REAPI run appears in the UI; whether a runner is recycled after a
     non-zero exit (undocumented, BB `docs/rbe-platforms.md:258-263`); whether input mtimes are
     preserved (`stat`); whether the workspace path is stable across recycled runs (`pwd`); and,
     by submitting a long `--remote_timeout`, the cloud's maximum timeout. Whether
     `CancelExecutions` works outside the CLI needs one more `grpcurl` call with BuildBuddy's
     protos, or waits for Slice 1.
- **Slice 1: the first product code, kept thin.** A REAPI backend for ADW code phases, placed by
  the operator. It contains: the REAPI client; capture with git semantics; the BuildBuddy
  invocation cancel (in slice 1, because Slice 1's real target is BuildBuddy, which has no
  `CancelOperation`); the `CargoJust` adapter; and remote-only placement of eligible code phases
  under `--exec-policy remote-only`. There is no fallback and no Local backend yet. Files are
  listed under "Implementation plan".
- **Slice 2:**
  - the Local backend through envd `ExecHost`, which closes F11 for routed phases;
  - the router with the four-way policy, fallback and the placement proptest (now that a second
    backend exists);
  - `cleanse` checkers through the router;
  - output modes.
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
  - Moving to any REAPI server, an SSH host or the local machine is a configuration change for
    correctness. It is not neutral for speed: the warm path relies on BuildBuddy's runner
    recycling, so another REAPI server gives cold runs unless it offers its own warm state
    (Part G).
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
  - two new crates and vendored REAPI/googleapis protos, plus vendored BuildBuddy protos for
    invocation cancel behind a feature;
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
| Action cache `Get/UpdateActionResult` | `:170`, `:193` | yes | A key that is not read-only, or a personal key of a Writer/Admin user, to write |
| `Execute` / `WaitExecution` (LRO stream) | `:118`, `:136` | yes | Merges identical in-flight actions |
| `CancelOperation` | `:99-101`, LRO `:88-101` | not found | No capability field announces it: call it and map `UNIMPLEMENTED` to `Unsupported`. On BuildBuddy, use invocation cancel (feature-gated code) |
| `Action`/`Command`/`Directory`/`Digest`/`Platform` | `:674`, `:749`, `:1048`, `:1177`, `:934` | yes | Properties in BuildBuddy vocabulary |
| `output_paths` | `:871` | yes | — |
| stdout/stderr digests, exit code, `server_logs` | `ActionResult` `:1250+` | yes | Live streams UNVERIFIED |
| `Action.timeout`, `do_not_cache`, `salt` | `:715`, `:719`, `:729` | yes | Cloud max timeout UNVERIFIED |
| `RequestMetadata.tool_invocation_id` | `:2622` | yes | Keys cancellation and the UI |

### Answers to the owner's ten questions (2026-10-08)

1. **Can current Cargo compilation run remotely without a Bazel migration?**
   - **Yes, for Linux targets, as whole commands.** One Action runs `just test-pkg <crate>` or
     clippy over the captured tree.
   - **No for the macOS developer binary on Linux executors.** The link needs the macOS SDK and
     Homebrew lld, and Apple SDK licensing on non-Apple hardware was not researched. On the free
     tier there are no managed darwin executors. BuildBuddy advertises managed Mac cores on paid
     tiers (Problem 3; terms UNVERIFIED), which could run macOS verification remotely; the binary
     the developer runs is still built and linked on this Mac.
2. **How?**
   - **Now:** `bb remote --script` (Slice 0).
   - **Owned:** a coarse REAPI Action from `omp-exec-reapi` in a digest-pinned Linux image, with
     `PYO3_CONFIG_FILE` pointing at a baked CPython and a recycled Firecracker runner for warm
     `target/` (Slice 1).
3. **Smallest valuable integration now:** Slice 0, with the Slice 0b probe beside it. Both are
   zero code: Slice 0 measures relief, and 0b checks the raw-REAPI assumptions before Slice 1 is
   written.
4. **Is BuildBuddy's direct REAPI suitable for arbitrary build/test commands?** Probably;
   UNVERIFIED until the Slice 0b probe runs. The source shows the protocol path: Execute needs no
   Bazel, and `bb execute` submits arbitrary argv. It does not show that BuildBuddy Cloud handles
   a 15-20 min action with gigabytes of disk. Caveats:
   - no stream-drop cancellation, and `Cancel` skips merged executions;
   - OCI runs with `network=off`;
   - whole-action cache granularity;
   - live logs, PTY, UI visibility and the cloud maximum timeout are UNVERIFIED.
5. **Bazel-specific vs standard REAPI:** see "BuildBuddy findings" 1-4.
   - **Standard REAPI:** CAS, AC, Execute/WaitExecution, ByteStream, capabilities, RequestMetadata.
   - **BuildBuddy-only:** the API-key header, `x-buildbuddy-platform.*` overrides, the property
     vocabulary, invocation cancel, the Run API, `bb remote`, Workflows, and BuildBuddy's BES
     ingestion and UI.
   - **Bazel-only:** BES itself (Bazel's Build Event Protocol), `.bazelrc` flags and
     `bb remote <bazel cmd>` output fetching.
6. **Auth model.**
   - An org or personal API key in `x-buildbuddy-api-key` on every gRPC call.
   - Org keys carry permission checkboxes (read-only, executor). A personal key is capped by its
     owner's user role (Admin, Writer, Developer, Reader), and those roles gate AC writes.
   - Certificate-based auth exists (BB `docs/rbe-setup.md:178`); how Cloud issues client
     certificates is UNVERIFIED.
   - omp keeps the key in the Keychain or the credential store and injects it as gRPC metadata
     only. `bb remote` (Slice 0) does not: the key is readable inside the runner (Part H.2).
7. **Capabilities on the current plan.** There is no account yet. Personal (free) per
   `pricing.tsx`: 80 Linux cores, 100 GB cache transfer, Workflows and BYO runners, no Mac cores.
   - **Answered by the docs:** snapshots hold the VM's whole disk and memory and their cache
     uploads and downloads are billed (Findings 4), so snapshot transfer counts against the 100 GB.
   - **`GetCapabilities` answers** (Slice 0b): whether zstd is on, digest functions, the batch
     limit, API versions.
   - **Still unknown without an account:** the cloud message-size limit, the maximum action
     timeout (a long `--remote_timeout` in Slice 0b shows it), managed arm64, quota numbers, how a
     same-host snapshot resume is metered, and whether `CancelExecutions` accepts a key outside the
     CLI flow (the CLI uses one).
8. **Linux limitations.**
   - amd64 by default; managed arm64 is UNVERIFIED.
   - The default image is 16.04, so a custom image is mandatory.
   - OCI has `network=off`; Firecracker has `external`; `host` is unavailable.
   - PTY is UNVERIFIED, so P6/P7 may not run.
   - Nested bubblewrap, landlock and docker tests are UNVERIFIED. Firecracker can run `dockerd`.
   - macOS `cfg` code is not exercised.
   - The Linux workspace suite has never been gated, so expect pre-existing failures.
   - Default workflow VMs (3 CPU / 8 GB / 20 GB) are too small for an 18 GB test target.
9. **What needs self-hosted executors** (or, for `darwin`, managed Mac cores on a paid tier,
   UNVERIFIED terms):
   - `darwin`: P12, P7's macOS case, the Seatbelt tests, macOS `cfg` coverage, release
     packaging;
   - `windows`;
   - possibly arm64 Linux;
   - `host`/`docker`/`podman`/`sandbox`/`none` isolation;
   - custom hardware.
   For relief, these must run on a machine other than this laptop.
10. **What stays portable to another REAPI provider:**
    - `omp-exec` (domain, adapters, placement, failures);
    - input capture and the Merkle builder;
    - the REAPI client and the `reapi` profile;
    - the output, telemetry and cancellation models.
    Not portable: the `buildbuddy` profile data (header, property names, recycling and isolation
    properties), the feature-gated invocation-cancel code, and Slice 0's `bb remote` usage.
    **Portable for correctness, not for speed:** without BuildBuddy's runner recycling every action
    is cold (Part G), so the near-term relief depends on BuildBuddy.

### Implementation plan (PR-sized, value first)

- **Slice 0 and 0b: no repository change** except, afterwards,
  `docs/audits/remote-verification-trial.md` recording the measurements, the snapshot transfer
  per run and the Slice 0b answers.
- **Slice 1, PR 1a: `crates/exec` (`omp-exec`, new).**
  - Contents: domain types (Part B), the `BuildSystemAdapter` trait and `ExecutionPlan`, the
    `CargoJust` adapter with its checked-in needs table, `ExecutionError` and its exhaustive
    `(ExecutionError, Stage)` classification, a minimal pure `place()` (operator-forced remote
    for eligible requests; `LocalRequired` refused remotely; no fallback), default deny globs,
    and strum enums with `vocab!` names for telemetry.
  - Also: a README, `[lints] workspace = true`, and an `area/env` entry in
    `.github/labeler.yml`.
  - Dependencies: `omp-core`, `omp-adw` (for `Requirement`/`NetworkScope`/`Posture`),
    `thiserror`, `strum`.
  - Tests: unit tests and the classification table test.
- **Slice 1, PR 1b: `crates/exec-reapi` (`omp-exec-reapi`, new).**
  - Protos are vendored under `crates/exec-reapi/proto/`:
    - REAPI v2 `remote_execution.proto`;
    - `semver.proto`;
    - googleapis `bytestream`, `longrunning/operations`, `rpc/status`, `rpc/error_details`, and the
      `api` annotations they import;
    - behind feature `buildbuddy-cancel`, the BuildBuddy protos needed for
      `BuildBuddyService.CancelExecutions`.
  - `build.rs` compiles them with protox plus tonic-prost-build, like `crates/proto/build.rs`.
  - Modules:
    - `profile`: `reapi` and `buildbuddy` as data, including toolchain → image + image env;
    - `tree`: manifest to canonical `Directory`;
    - `cas`: FindMissing, batch, ByteStream, zstd;
    - `execute`: Execute, the WaitExecution reattach, `CancelOperation`, and result fetch;
    - `capabilities`;
    - `buildbuddy_cancel` (feature-gated): invocation cancel.
  - The backend reads blob bytes through a `BlobSource` trait implemented over envd's blob service,
    so it holds no second copy.
  - Its own `area/env` entry in `.github/labeler.yml` (`AGENTS.md`: a new crate gets exactly one
    `area/*` entry).
  - Licence review of every vendored proto, BuildBuddy's included, is part of the PR. Their
    licences were not checked in this pass (UNVERIFIED).
- **Slice 1, PR 1c: capture in envd.**
  - `crates/envd/src/workspace/operations.rs`: a capture-only entry point next to `snapshot_at`,
    with git path sets from `omp-vcs`, the deny filter applied before `hash_file`, the
    `omp_secrets::credential_rules()` content scan, no transition lock and no published
    generation (Part D).
  - `crates/proto/proto/omp/env/v1/env.proto`: a `CaptureInputs` request and reply beside
    `SnapshotWorkspace` (`:954`), and the matching method in `crates/env/src/client.rs` beside
    `snapshot_workspace` (`:1500`).
- **Slice 1, PR 1d: wiring.**
  - `crates/driver/src/exec/{mod.rs,settings.rs}` (new): `sv_exec_*` convars (Part F) with the TLS
    validate hook, backend composition (the `Backend` enum), and credential resolution through
    `CommandCredentials`.
  - `crates/driver/src/adw/production.rs`:
    - `ProductionAdwHost::open` composes an envd environment, which capture, the `BlobSource` and
      `CommandCredentials` all need; today it holds only the root, the context and the resolver
      (`:50-77`);
    - `run_command` builds a `BuildCommand`, plans it through `CargoJust`, and sends eligible
      requests to the REAPI backend; everything else keeps today's local path until slice 2;
    - `AdwHost` reports posture per dispatch and per placement (Part C.2).
  - `crates/driver/src/adw/definition.rs`: the optional per-phase `placement = "local"` field.
  - `crates/app/src/adw_cmd.rs`:
    - use `process_ctx_with` (cfg finally applies);
    - wire Ctrl-C to the cancel token;
    - add `--exec-policy`;
    - print the placement line.
  - `crates/observability/src/{semconv.rs,attrs.rs}`: `omp.exec.*`.
  - `scripts/exec-image/` (new): the image recipe and a build script that derives pins from
    `rust-toolchain.toml`, `fetch-python.sh` and `ci.yml`.
  - `.omp/workflows/verify.toml` (new): `just test-pkg omp-envd` and `just clippy` phases.
- **Slice 2:**
  - the Local backend over envd `ExecHost`;
  - the router's four-way policy, fallback and the placement proptest;
  - `crates/driver/src/cleanse/production.rs` routed;
  - output modes.
- **Slice 3:** an SSH backend in `crates/envd` (it owns `russh`) behind the same trait.

### Test plan (owning seams)

- **`omp-exec`** (slice 1).
  - A failure-classification table test: every gRPC code in REAPI `:87-101` × stage (before or
    after acceptance), against the exhaustive `match`.
  - `CargoJust` needs-table tests: `e2e-p12` is never remote-eligible, `fmt` is `LocalRequired`,
    an unknown recipe is `LocalPreferred`, and a project `placement = "local"` only narrows.
- **`omp-exec`** (slice 2). A placement proptest:
  - `LocalRequired` never goes remote;
  - `RemoteRequired` never goes local, under every policy;
  - no placement violates the request's `Requirement`, including a fallback placement;
  - an exit ≠ 0 never falls back;
  - nothing falls back after acceptance;
  - `Cancelled` never falls back.
- **`omp-exec-reapi`.**
  - Golden `Directory`/`Action` digests against REAPI canonical-encoding vectors.
  - Golden `Command` encoding: a root `cwd` becomes an empty `working_directory`, never `"."`.
  - `sv_exec_remote_cache = off` sends `skip_cache_lookup=true` and `do_not_cache=true`.
  - The endpoint validate hook refuses `grpc://` to a non-loopback host.
  - A **fake in-process REAPI server** (tonic) with CAS, AC, Execute and WaitExecution, plus
    injectable faults:
    - a `MISSING` precondition;
    - `UNAVAILABLE`;
    - `RESOURCE_EXHAUSTED`;
    - a stream drop with reattach;
    - `DEADLINE_EXCEEDED`;
    - `CancelOperation` answering `UNIMPLEMENTED`;
    - a fake `CancelExecutions` (feature `buildbuddy-cancel`).
  - Bounded waits; RAII-owned servers.
- **`omp-driver`.** An ADW code phase routed to the fake backend:
  - exit 0 accepts;
  - exit 1 rejects with bounded feedback;
  - an infra fault gives a typed failure that aborts the run, never a local rerun (slice 1);
  - Ctrl-C produces `Cancelled` and a cancel request, and stops the run instead of feeding
    "cancelled" back as a correction;
  - from slice 2: an infra fault before acceptance plus `remote-preferred` and
    `sv_exec_fallback_local` gives a local fallback; after acceptance it gives reattach, then a
    typed failure.
- **`omp-envd`.** Capture:
  - the captured path set equals `git ls-files` (tracked, including a force-added ignored file)
    plus `git ls-files --others --exclude-standard`, minus a deleted file, on a fixture repo;
  - a `.ignore` file has no effect;
  - an untracked `.env` is excluded and counted; a tracked `.env` refuses the run; a content-scan
    hit behaves the same;
  - capture publishes no generation and succeeds while a workspace transition is in progress.
- **Live tests** are gated behind `OMP_EXEC_LIVE_ENDPOINT` plus `OMP_EXEC_LIVE_CREDENTIAL` (a
  credential source, never a value) and skipped otherwise. They never run in CI by default.

### E2E proofs to add

- **P13: remote verification over a fake REAPI server.**
  - `omp adw run` with a remote-placed code phase.
  - It proves: capture, dedupe on the second run, result, cancel, no secret uploaded, and that a
    file deleted between two runs is absent from the second run's input root.
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
  - `/usr/bin/time -l`: wall and user+sys CPU of the whole tree. Its maximum resident set size is
    the largest *single* descendant, not the combined peak of many concurrent `rustc` and link
    processes, so it does not measure RAM relief on its own;
  - system-wide memory sampled once a second during the run: `vm_stat` pageouts and swapouts,
    `sysctl vm.swapusage`, and the summed RSS of the recipe's process tree;
  - the `target/` size delta;
  - with the CI runner service idle and recorded.
- **Remote arms:**
  - cold: a new snapshot key or fresh runner;
  - warm: a recycled runner.
  - No action-cache arm: the action cache stays off while warm runners are in use (Part G).
- **Per remote run:** local capture, FindMissing, upload bytes and time, queue, input fetch,
  execution, output upload, download (from `ExecutedActionMetadata`), local CPU and memory of the
  client sampled as above, and the BuildBuddy cache transfer of the run (snapshots included).
- **Success criterion for Slice 0/1:**
  - the client's local CPU time and its peak memory are each under 5% of the local recipe's;
  - warm remote wall time is within 1.5× of warm local wall time;
  - cumulative cache transfer stays inside the Slice 0 stop condition.
  Local wall time is not a criterion, because the client waits for the remote run. The thresholds
  are owner adjustable.
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
  deny list, the content scan, untracked-file policy, operator-only enablement, and pre-run
  listing in Slice 0.
- **Credential exposure in Slice 0.** `bb remote` hands the API key and a GitHub App token to
  every process in the runner, including third-party build scripts and tests, with external
  network (Part H.2). A stolen key with AC write could forge cached results. Mitigations: a
  dedicated organization and key, deleted after the trial; the trial organization's action cache
  never gates a verdict; `unset` at the top of the script.
- **Action-cache poisoning.** Any holder of a key with AC write can write entries, and warm
  runners are not hermetic. Mitigation: the action cache is off whenever warm state is used, and
  an AC hit is never an acceptance (Part G).
- **Linux failures that are not offload failures.** The Linux suite was never gated (`ci.yml:338`
  is macOS only).
- **Warm state does not hold.** Runner recycling is best-effort, so remote cold runs at default
  sizes can be slower than local.
- **Relief is tied to BuildBuddy for now.** The warm path uses BuildBuddy-only properties; another
  REAPI server gives correct but cold runs (Part G).
- **Leaked remote work after cancel** if invocation cancel fails, or for an execution another
  request merged onto. It is bounded by `Action.timeout` (30 min by default).
- **Cost and quota surprises.** Snapshots count as billed cache transfer and can be tens of GB
  each (Findings 4); Team pricing has placeholders. Mitigation: the Slice 0 sizing, save policy
  and stop condition.
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
  - whether a runner is recycled after a non-zero exit;
  - how a same-host snapshot resume is metered (that snapshots are billed transfer is documented);
  - Team prices, and the terms of managed Mac executors.
- The scope of the GitHub App token BuildBuddy passes to the runner.
- Everything in the Slice 0 and 0b sketches: none of it was run.
- Whether `bb remote --container_image` accepts the `ubuntu-24.04` alias.
- Pants or Reclient on BuildBuddy.
- The licenses of the vendored protos.
- Apple SDK licensing for cross-builds.

### Open questions for the owner

1. **R0.** Should this laptop keep serving omp CI, and the two other runner services? It is likely
   the largest load.
2. **Disclosure.** Is uploading the omp2 source to BuildBuddy Cloud acceptable, and is linking the
   private repo to BuildBuddy acceptable for Slice 0, given that the key and a GitHub App token
   are readable inside the runner?
3. **Image hosting.** Where is the image hosted (public registry or private with registry
   credentials), and who rebuilds it?
4. **Scope of Linux remote verification.** Should it become a CI gate (closing the `AGENTS.md:86`
   discrepancy), or stay a developer tool?
5. **Budget.** Is Personal-tier capacity (80 cores, 100 GB transfer, snapshots included) enough,
   or Team? Should BuildBuddy be asked about managed Mac cores (R4b) for the macOS proofs?
6. **`.gitignore`.** Change `/vendor/` to `/vendor`, so the worktrees' `vendor` symlink stops
   showing as untracked? It is a separate one-line change.

### Review record (2026-10-08)

Two technical reviews of the first draft (`b47e6dea02`) were checked against the cited code and
sources, and every finding that held was applied above. Two points were not applied as stated:

- **Move the plan, test, risk and Q&A sections out of `## Status in omp`.** Not applied. ADR 0039
  uses the same layout (`docs/adr/0039-remote-control-and-factory-modes.md:542-747`), and the
  section still opens with the status label and names real paths, as `docs/adr/README.md:30-38`
  asks.
- **"The REAPI client in `crates/driver` contradicts ADR 0006 and AGENTS.md."** Not applied as
  stated. Driver is where network service clients are composed (`AGENTS.md:186-188`), as the
  inference services are, and those already send project content to third parties. The parts
  that held were applied: capture and blob reads stay in envd behind the env protocol, `omp adw
  run` must compose an envd environment, and agent-initiated runs need operator approval
  (Part A.3, PR 1c and 1d).

One correct finding is still open: `docs/adr/README.md` lists 0039 (`:59`) but not 0040. This
change was limited to this record and its STATUS.md row, so the index line is a follow-up.

## References

- ADRs 0001, 0003, 0006, 0007, 0009, 0010, 0011, 0028, 0033, 0035, 0039.
- Code:
  - `crates/envd/src/workspace/operations.rs`
  - `crates/envd/src/exec.rs`
  - `crates/driver/src/adw/production.rs`
  - `crates/driver/src/cleanse/checkers.rs`
  - `crates/adw/src/profile.rs`
  - `crates/driver/src/adw/{mod.rs,definition.rs}`
  - `crates/vcs/src/lib.rs`
  - `crates/walker/src/lib.rs`
  - `crates/secrets/src/builtins.rs`
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
  - `https://www.buildbuddy.io/docs/remote-runner-introduction/` (snapshot cost and save policy)
  - `https://www.buildbuddy.io/pricing` (fetched 2026-10-08)
  - BB `enterprise/server/hostedrunner/hostedrunner.go` and
    `enterprise/server/cmd/ci_runner/main.go` (runner credentials)
  - `https://buck2.build/docs/users/remote_execution` (Buck2 against BuildBuddy)
  - `https://github.com/bazelbuild/remote-apis` (client and server lists)
