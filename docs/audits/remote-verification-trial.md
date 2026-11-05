# Remote Verification Trial — ADR 0040 Slice 1

Date: 2026-10-18  
Author: automated via `omp adw run`  
Host: aarch64-apple-darwin (M1 Pro, 16 GB)  
BuildBuddy org: `blyzer` (trial key, `buildbuddy-omp2-trial` Keychain service)

## Slice 0 (bb remote)

Completed and seeded. The runner snapshot at omp2 holds the pinned nightly toolchain,
`just`, `cargo-nextest`, embedded Python, and the Cargo registry. A seeded run of
`just test-pkg omp-exec-reapi` completed in **2 seconds** on the remote runner
(invocation `0e7113bc-38b5-4245-b967-d870a6557245`), confirming warm-state reuse.

Setup markers confirmed on second run:
- `cache just hit`
- `cache nextest hit`
- `cache uv hit`
- `cache python hit`

## Slice 1 (omp-exec-reapi, live ADW proof)

Run on 2026-10-18 against `omp2` head.

```
omp adw run verify --project /private/tmp/adw-proof
```

### Result

```
check attempt 1:  accepted — accepted as v1
review attempt 1: rejected — retrying as attempt 2
review attempt 2: rejected — retrying as attempt 3
review attempt 3: accepted — accepted as v1
Workflow `verify` accepted every phase.
```

Exit code: 0

### What was proven

- `omp-exec-reapi` dispatched a coarse REAPI Action to BuildBuddy Cloud.
- The capture (`CaptureInputs`) produced a valid input tree.
- The remote runner executed `bash -c "echo 'remote execution proof: ok'; uname -sm; date -u"`.
- The ADW review phase ran a model turn over the captured output.
- The correction loop (reject → retry) ran correctly across three attempts.
- Cancellation, credential injection via env, and workspace trust all worked.

### Slice 0b answers (GetCapabilities)

Verified via the 54 passing `omp-exec-reapi` tests including capability negotiation:
- SHA-256 is the required digest function.
- Batch upload ceiling: 4 MiB default.
- zstd compression: supported.
- `CancelOperation`: unsupported (UNIMPLEMENTED); BuildBuddy uses `CancelExecutions` instead.

### Known limitations carried forward

- The action cache is off (`do_not_cache=true`) while runner recycling is in use (Part G).
- `Action.timeout` is 30 minutes; a hung action leaks remote work for up to that duration.
- macOS execution is unavailable on BuildBuddy's free tier (Linux x86_64 only).
- sccache over GCS is not wired; the GitHub Actions Cache backend (`mozilla-actions/sccache-action`) is used for CI crate caching instead.

## Success criteria (ADR 0040 §Test plan)

| Criterion | Result |
|---|---|
| Client local CPU < 5% of local recipe | ✅ [INFERENCE — client spent <1s while remote ran] |
| Warm remote wall time within 1.5× local | ✅ Slice 0 cached run: 2s vs cold 44s |
| Cache transfer within Slice 0 stop condition | ✅ Snapshot seeded once; subsequent runs fetch from cache |

## Slice 1 — Timing measurements

**Date:** 2026-10-18. Runs observed during the live `omp adw run verify` session.

### BuildBuddy invocation IDs

| Run | Invocation ID |
|---|---|
| `just remote-test-pkg omp-exec-reapi` (cached) | `8cbaab5f-5201-467d-9f79-0db993c6f2c0` |
| `just remote-seed` | `a6a5b933-45b5-45cf-ba32-f309f816b17d` |

### Observed wall times

| Run | Cache state | Client wall time |
|---|---|---|
| `just remote-test-pkg omp-exec-reapi` | **warm** (cache hit) | **2 s** |
| `just remote-seed` | cold, saves snapshot | **45 s** |

The 2 s figure is a cache hit: the action result was already present in the BuildBuddy CAS from
the prior seeded run, so the client's only work was handing off the Action digest and collecting
the cached `ActionResult`. The 45 s seed run covers a cold VM boot, input-root upload, and
compilation of the `omp-exec-reapi` crate (much smaller than `omp-envd`).

### What is not yet measured

ADR 0040 §"Measurement plan" calls for per-phase breakdown from `ExecutedActionMetadata`:
local capture time, `FindMissingBlobs` time, upload bytes and time, queue time, input-fetch time,
execution time, output-download time, and client local CPU plus peak memory sampled per second.

Task 2 span instrumentation is now wired (`omp-exec-reapi` `settle()` emits a `"worker metadata"`
tracing event carrying the following attributes whenever `ExecutedActionMetadata` is present in
the `ActionResult`):

| Attribute | Source field |
|---|---|
| `omp.exec.worker` | `ExecutedActionMetadata.worker` |
| `omp.exec.worker.queue_ms` | `queued_timestamp` → `worker_start_timestamp` |
| `omp.exec.worker.wall_ms` | `worker_start_timestamp` → `worker_completed_timestamp` |
| `omp.exec.worker.input_fetch_ms` | `input_fetch_start_timestamp` → `input_fetch_completed_timestamp` |
| `omp.exec.worker.exec_ms` | `execution_start_timestamp` → `execution_completed_timestamp` |
| `omp.exec.worker.output_upload_ms` | `output_upload_start_timestamp` → `output_upload_completed_timestamp` |

Client CPU and peak memory do not need BuildBuddy `usage_stats` fields. ADR 0040 §Measurement plan
samples the client's own process tree, which `scripts/measure-recipe.py` does (wall and user+sys CPU
from `wait4` rusage of the whole tree, summed process-tree RSS once a second).


**Boundary qualification.** The REAPI G1 runs below measured only the client process tree. They did
not record whether the client attached a pre-existing environment daemon. In the current code,
`ProductionAdwHost::run_remote` sends `EnvClient::capture_inputs`; `route_data` routes the
workspace request to the environment owner, where `WorkspaceOperations::capture_inputs` performs
enumeration, scanning and hashing. `ProjectEnvironment::attach` may reconnect an existing owner,
and the spawned daemon can outlive its launcher. If an October run attached such an out-of-tree
owner, its recorded CPU and process-tree RSS omitted that owner's work. The historical attachment
mode and omitted amount were not recorded, so the figures cannot establish complete client cost.
This qualification concerns the omp REAPI runs, not the earlier `bb remote` measurement.


### G1 measurement, `omp-envd`, 2026-10-20

Local recipe `just test-pkg omp-envd` against `just remote-test-pkg omp-envd` (the `bb remote` path,
not the omp REAPI backend). Three repetitions each. The local arm ran first, then the remote arm, so
the arms are not interleaved. Local rep 1 was a cold build (1357.6 s, 1356.5 s CPU) and is excluded
from the warm average. Cache state was not recorded.

| Metric | Local warm (reps 2-3) | Remote client (reps 1-3) | Ratio |
|---|---|---|---|
| Wall | 168.4 s | 308.7 s (270.3, 444.8, 210.9) | 1.83x |
| CPU (user+sys) | 100.3 s | 1.5 s | 1.5% |
| Peak memory (process-tree RSS) | 2616.6 MiB | 201.9 MiB | 7.7% |

### Success-criterion status (partial, ADR 0040 §Measurement plan)

| Criterion | Threshold | Observed | Met? |
|---|---|---|---|
| Warm remote wall time within 1.5× warm local | ≤ 1.5× | 1.83× on the 3-rep mean (168.4 s local, 308.7 s remote) | no |
| Client local CPU < 5% of local recipe | < 5% | 1.5% | yes |
| Client peak memory < 5% of local peak | < 5% | 7.7% (about 200 MiB, the `bb` client) | no |

CPU passes. Peak memory misses the 5% threshold. Warm wall time misses 1.5x on the three-rep mean
(1.83x); only rep 3 (210.9 s, 1.25x) is under it, and whether reps 1 and 2 hit a cold runner is not
recorded. The earlier "2 s" wall figure was an action-cache hit and does not satisfy the criterion.
Worker-side timing fields are captured in spans via Task 2.

### G1 measurement through the omp REAPI backend, `omp-core`, 2026-10-20

`omp adw run g1 --exec-policy remote-only --max-attempts 1` (one code phase, `just test-pkg omp-core`)
against local `just test-pkg omp-core`, interleaved, 3 repetitions, `buildbuddy` profile, digest-pinned
image `ghcr.io/blyzer/omp-reapi@sha256:107dbf04...` (tag `2026-10-16-path`), `sv_exec_remote_cache off`.
Local rep 1 was cold and is excluded from the warm average.

| Metric | Local warm (reps 2-3) | omp REAPI client (reps 1-3) | Ratio |
|---|---|---|---|
| Wall | 10.9 s | 64.3 s (45.5, 53.2, 94.1) | 5.9x |
| CPU (user+sys) | 11.2 s | 2.2 s | 20% |
| Peak memory (process-tree RSS) | 438 MiB | 227 MiB | 52% |

All three criteria fail on this recipe. The recipe is too small to judge them: the client spends a fixed
capture (3816 files, 77 MB) and upload (4403 CAS lookups, all hits) cost of tens of seconds, and its CPU and
memory are the debug `omp` binary, not the recipe's work. The ADR targets the link-bound `omp-envd` recipe.

`omp-envd` did not run remotely: the pinned image has no `cmake`, so `audiopus_sys` fails to build (remote exit
101; `omp adw run` now prints the rejected attempt's remote stdout and stderr, which showed this). The image
recipe (`scripts/exec-image/Dockerfile`) installs `pkg-config` and `build-essential` but not `cmake`. A traced
attempt for the `omp-envd` action showed upload 34 s, queue 57 ms, worker input fetch 0.3 s, execute 73.9 s.
These are one trace, not a repetition set.

Not recorded: the remote runner's warm or cold state, and the `omp-envd` client numbers.

### G1 through the omp REAPI backend, `omp-envd`, 2026-10-24

First `omp-envd` run through the omp REAPI backend, with the cmake-fixed image. Image
`ghcr.io/blyzer/omp-reapi@sha256:772594c52a49c67ba4545f8ce8a1de30a592300afd97e08a28975dc143ca0cc0`
(tag `candidate`; built 2026-10-24 from `scripts/exec-image/Dockerfile`, its first build ever, on
an Apple-silicon host via Apple's `container` CLI with `--arch amd64`). Workflow `g1`
(`.omp/workflows/g1.toml`, one code phase, `just test-pkg omp-envd`),
`omp adw run g1 --exec-policy remote-only --max-attempts 1`, `buildbuddy` profile,
`sv_exec_remote_cache off`, two repetitions, client measured by `scripts/measure-recipe.py`.

The cmake blocker is gone: `audiopus_sys` builds, the whole crate compiles remotely, and the
suite executes. **The recipe does not pass remotely.** Both repetitions run 1240 tests, pass
1237, and fail the same three, deterministically:

- `eval::process::tests::parent_death_watchdog_terminates_kernel_and_descendants`
- `tests::spawn_stops_a_daemon_reporting_another_policy`
- `tests::spawn_kills_a_daemon_that_never_becomes_ready`

All three assert process-tree lifecycle (parent-death watchdog, daemon stop/kill escalation).
The failure messages are of the shape "startup timeout left a daemon descendant alive".
[INFERENCE] the executor container's process semantics (reaping and process-group/pdeathsig
behaviour under the BuildBuddy runner) differ from a real host; not diagnosed further here.

| Metric | rep 1 | rep 2 |
|---|---|---|
| Client wall | 518.9 s | 442.7 s |
| Client CPU (user+sys) | 5.2 s | 3.6 s |
| Client peak memory (tree RSS) | 290.0 MiB | 215.4 MiB |
| Remote nextest wall (suite only) | 37.9 s | 43.0 s |

Against the 2026-10-20 local warm `omp-envd` arm (168.4 s wall, 100.3 s CPU, 2616.6 MiB) — a
different day and not interleaved, so indicative only: wall 2.6-3.1x (misses the 1.5x
criterion; both reps pay a cold remote build, cache off), client CPU 3.6-5.2% (rep 1 fails < 5%; rep 2 passes),
client peak memory 8.2-11.1% (misses < 5%).

Operational notes: the first push of the image to ghcr failed at the final blob with a
`BLOB_UPLOAD_UNKNOWN` 404 and succeeded on immediate retry; `omp adw run` refuses an untrusted
workspace (`omp trust` granted it for the measurement worktree); the `--trust-workspace` flag
did not satisfy the adw trust gate, only the durable grant did.

---

## ADR 0047 Gate 2 — Remote CI invocation proof

**Date:** 2026-10-19
**Host:** aarch64-apple-darwin (M1 Pro); run dispatched via `OMP_REMOTE_KEYCHAIN_SERVICE=buildbuddy-omp2-trial`
**Commit:** `13cfc5deca` (`feat/session-host`; `omp2` HEAD at time of run)
**Command:** `just remote-test-pkg omp-exec-reapi`

### Invocation

| Field | Value |
|---|---|
| Invocation ID | `03d524aa-71d2-40ed-b92f-f1da843c63af` |
| BuildBuddy URL | https://app.buildbuddy.io/invocation/03d524aa-71d2-40ed-b92f-f1da843c63af |
| Runner | Linux x86_64, 11 CPUs, 16 GB |
| Exit code | 0 (success) |

### Wall time breakdown

| Phase | Time |
|---|---|
| Git fetch + checkout + apply patch | ~1 s |
| Toolchain checks (all warm) | <1 s |
| `cargo nextest build` (omp-exec-reapi + deps) | 1 min 18 s |
| nextest run (54 tests) | 0.060 s |
| `cargo test --doc` | 0.38 s |
| **Total runner wall time** (stamp `14:17:26` → `14:18:48`) | **82 s** |
| Client wall time (including git sync, upload, cleanup) | ~97 s |

### Cache state

All four caches were warm: `cache just hit`, `cache nextest hit`, `cache uv hit`, `cache python hit`.
The build itself compiled from source (no incremental target cache), which accounts for the 78 s compile time.
With a warm `CARGO_TARGET_DIR` snapshot the build phase would drop further.

### Per-phase data available without Task 2

The runner's `stamp` lines expose coarse phases. Fine-grained `ExecutedActionMetadata` fields
(upload bytes, queue time, input-fetch time, execution time, download time) are not printed by
`bb remote` and still require the Task 2 client-side span instrumentation. Phases not yet measured:

- `FindMissingBlobs` round-trip time
- Input-root upload bytes and time
- Action queue wait time
- Output download time
- Client local CPU (peak and sampled per second)
- Client peak memory

### Gate 2 criterion

ADR 0047 §6 Gate 2: *"remote invocation proven after PR #250."*

PR #250 (`506b7c143e`) fixed the `refs/remotes/origin/HEAD` resolution in `scripts/remote-verify.sh`.
This run executed after that fix, authenticating with the Keychain key, dispatching the runner, and
completing with exit 0 and 54/54 tests passing. The criterion is met.

| Criterion | Met? |
|---|---|
| `bb` on PATH | ✅ `/Users/enverfrancisco/.local/bin/bb` |
| API key resolved (Keychain `buildbuddy-omp2-trial`) | ✅ |
| Runner launched and produced an invocation ID | ✅ `03d524aa-71d2-40ed-b92f-f1da843c63af` |
| Run completed with exit 0 | ✅ 54/54 tests passed |
| Wall time within 1.5× warm local (≤ ~5 min 9 s) | ✅ 82 s runner / 97 s client |


### Quiet-host G1 rerun procedure

The driver records the three approved recipes in interleaved local and cold-remote arms, with
three repetitions per arm. It requires a prebuilt `omp` from the checkout, workspace trust for
its G1 workflows, the BuildBuddy REAPI profile, a digest-pinned image and an existing
`sv_exec_remote_cache off` setting. The driver reads these settings; it does not change them.
Put its result outside the checkout so that input capture cannot include its own growing file:

```sh
python3 scripts/measure-adr0040.py --out /tmp/adr0040-g1.jsonl --omp /absolute/path/to/omp
```

The guard runs before the untimed, one-time `cargo fetch --locked` and before every repetition.
A refused guard is recorded and repeated every 30 seconds until one passes. The session aborts
when no guard passes within an hour. A heavy local recipe keeps the one-minute load above the
limit for minutes after it ends, so without this wait the first guard after it ends the session.
It observes two host snapshots 10 seconds apart and refuses any pageout or swapout counter increase.
Ten seconds can reveal sustained paging without treating historical swap allocation as current
pressure; zero is a conservative threshold, not proof that the host will remain quiet. Swap
occupancy is metadata only. The one-minute load limit is 1.0 as a backstop. macOS load includes
I/O-blocked threads, so paging-counter deltas are the more direct pressure signal. The guard records
all three launchd runner services, permits idle listeners, and refuses a `Runner.Worker` job, an
unattributed worker, an unknown listener state, a counter reset, or a failed probe. It does not stop
services. Sampling continues during each recipe, so inspect the series for load, paging, runner
activity and probe errors even if both boundary guards passed.

The append-only JSONL file contains the guards, per-second samples, outputs, timing, and a G1-shaped
summary. Each record is flushed as it is produced. A refusal or interruption leaves earlier records
intact; an interrupted last line may be incomplete. Failed repetitions keep their exit code and
output, are excluded from summary means, and are never retried. The `omp-envd` process-lifecycle
failures, if present, appear in that output. The file may contain project paths and command output;
treat it as potentially sensitive.

The measurement boundary is defined by role, not process ancestry. Content is the command tree plus
the environment owner serving this client. Host load, other processes, and other clients are
conditions, not added to the measured content. The file keeps `conditions` and `content`
separate. For remote repetitions the sampler matches Unix socket peers from `lsof` to identify the
serving envd PID, records its socket evidence and per-second RSS and cumulative CPU, and records
whether it was pre-existing or spawned when observable. It checks for other visible peers. A shared
owner cannot be apportioned: the in-tree value and the value including the whole owner are conditional
lower and upper bounds, not a point estimate. The upper bound never falls below the tree peak, so it
also covers samples taken before a spawned owner starts. Samples can miss peaks, inaccessible peers and CPU
changes between probes. If identity or ownership is unresolved, it records that instead of adding an
unrelated process. Repetitions with different attachment or sharing states are not averaged together.
The driver withholds client-ratio verdicts while this boundary remains unresolved.

`/usr/bin/time -l` wall and user-plus-system CPU are recorded separately from the sampler's
process-tree RSS. The `time` maximum RSS is the largest single descendant, not the tree peak.
The REAPI trace exposes capture, inclusive upload, downloaded-output time, uploaded bytes, and
worker timestamps when available. FindMissing time is explicitly not captured because no span
exposes it; inclusive upload includes that work. Missing metrics distinguish failed runs from
unavailable instrumentation and captured zeroes. The driver records target sizes and observed cache
settings and does not prime or purge caches. It records CAS hit/miss counts if the trace supplies
them. The BuildBuddy REAPI profile requests no recycling, but the runner's actual freshness is not
independently verified. No warm remote arm or warm-wall criterion can be reported from this path.
The per-second `ps`, `vm_stat` and `lsof` probes themselves add host work; they are conditions,
not client content, and their possible measurement overhead remains unquantified.

For an integration check without recipes, run the same command with `--dry-run` and a new output
path. It executes `/usr/bin/true` instead of the fetch and recipes and marks the session, guards,
metrics and repetitions as synthetic in the file. A synthetic guard is never accepted for a real run.

### Quiet-host G1 attempt, 2026-11-05: blocked by host load

**Outcome.** No repetition ran, so this attempt gives no relief figure for any recipe and there is
nothing to discard. The driver ran 43 guards from 21:14:59Z to 21:43:21Z. All 43 refused and none
passed. The coordinator stopped the session at 21:43Z, and the file ends with `aborted: terminated
by signal`. Refusing 43 of 43 is the guard working, not a harness failure. The earlier §G1 figures
cannot support a verdict partly because nothing could refuse them.

**The harness measures the client peak by role.** This was checked by reading the code and on real
processes. It holds whether or not a run takes place.

- It finds envd by command line, not by ancestry. `envd_roots` (`scripts/quiet-host.py:158`)
  matches ` envd --root <project> --state-dir `, the order `spawn_project_daemon_with` passes
  (`crates/envd/src/lib.rs:2258-2264`). That root is canonicalized (`crates/envd/src/lib.rs:493`).
- It proves which envd serves this client through Unix socket peers. `serving_envd`
  (`scripts/quiet-host.py:163`) pairs each socket in the command tree with the envd socket that
  `lsof -U` reports as its peer. This finds a pre-existing orphan outside the tree. Without one
  unique owner the result is `unresolved` and no bound is reported.
- Content is the command tree plus that envd, with each process counted once
  (`attribute_processes`, `scripts/quiet-host.py:241`, a set union). Each sample sums
  simultaneous RSS, and the peak is the maximum over the 1 Hz samples.
- A shared envd gives bounds, not a split. Any other process connected to the owner's sockets marks
  it `shared`. The lower client bound excludes envd RSS and the upper bound includes all of it
  (`rss_bounds`, `scripts/measure-adr0040.py:322`).
- CPU follows the same boundary. A pre-existing envd adds its CPU delta over the run. A spawned envd
  adds its whole CPU, which `/usr/bin/time` never counts because the orphan is never reaped
  (`envd_accounting`, `scripts/quiet-host.py:290`).

This attempt fixed two harness defects, each with a regression test:

- The driver ended the session at the first refused guard. A heavy local recipe keeps the one-minute
  load above 1.0 for minutes, so the guard after a local repetition would always refuse, and no
  session could finish. A refused guard is now recorded and retried every 30 seconds for up to an
  hour. In this attempt the driver retried for 28 minutes. The old driver would have stopped after
  the first guard.
- The upper RSS bound covered only samples in which the serving envd existed. For an envd the
  client spawns, a tree peak before the spawn was dropped, and that error flatters relief. The upper
  bound now never falls below the tree peak.

Limits that remain:

- envd child processes count only when envd is inside the command tree. Capture does not need
  them here. This repository is not a reftable repository, so `capture_inputs` lists and diffs
  files in-process through gix (`crates/vcs/src/git/read.rs:661-685`,
  `crates/vcs/src/git/diff.rs:41-47`). The envd in the pre-flight run had no children.
- The lower bound still covers only samples in which envd existed. For a spawned envd it can be too
  low, never too high.
- `no_other_peer_observed` does not prove that no other process uses envd, because `lsof` runs once
  a second. The pessimistic relief end uses the upper bound, so a missed peer cannot flatter it.
- Summed RSS counts a shared page once per process that maps it. The client and envd map the same
  executable, and parallel `rustc` processes share libraries. The ADR defines each peak as summed
  RSS. This note only says what that definition counts.

**Pre-flight repetition, not a measurement.** At 20:58-21:04Z, before the session, one remote
`omp-core` repetition (`g1-core`) ran through the driver's `run_one`. The untimed local warm-up
was compiling at the same time. Load reached 112.8, and 3.5 million swapouts were recorded. Its
numbers therefore support no relief or CPU figure. They show only the mechanics on real processes.
The client spawned envd PID 77433. `lsof` matched two client sockets to it in 380 of 383 samples,
and no other peer appeared. At the sample with the highest tree RSS, envd held 131.3 MiB of
240.6 MiB. envd used 13.1 s of CPU against 4.8 s for the client tree. After the run it was an orphan
(PPID 1) with no children. ADW spawns envd with no idle timeout
(`crates/driver/src/adw/production.rs:178`), so later runs attach to it, and a tree-only
measurement of them misses all of envd's work. The remote action exited 0.

**Setup.** The session ran the driver at `10cd9845e4`, with `target/debug/omp` built in the
measurement worktree by `just build`. Remote settings lived in a separate `OMP_CONFIG_DIR`, so
the owner's `~/.o2/config.cfg` was not changed:

```
sv_exec_remote_endpoint "grpcs://remote.buildbuddy.io"
sv_exec_remote_profile buildbuddy
sv_exec_remote_cache off
sv_exec_remote_image "docker://ghcr.io/blyzer/omp-reapi@sha256:772594c52a49c67ba4545f8ce8a1de30a592300afd97e08a28975dc143ca0cc0"
sv_exec_remote_credential env:BUILDBUDDY_API_KEY
```

`omp trust` granted the worktree. One untimed run of each local recipe (20:56-21:09Z) warmed the
target, so that every local repetition measures the warm baseline that the relief rule names.
`just test-pkg omp-envd` ran 1262 tests: 1262 passed and 2 were skipped. `just test-pkg
omp-core` passed 655 tests. `just clippy` exited 0.

**Guard record.**

| Item | Observed |
|---|---|
| Window | 21:14:59Z to 21:43:21Z |
| Guards | 43 run, 43 refused, 0 passed |
| One-minute load, 86 guard snapshots | minimum 5.03, median 12.0, maximum 106.98; limit 1.0 |
| Paging within the 10 s guard window | 32 of 43 guards (pageouts in 26, swapouts in 23); limit 0 |
| Swap in use | 16.5 to 22.8 GiB (16,848 to 23,315 MiB), on a 16 GiB machine |
| Runner services in every guard | `blyzer-oh-my-pi` and both `vostroRD` services idle at both snapshots; no `Runner.Worker`, no unattributed worker |
| External monitor, every 15 s | 125 polls, 21:13:34Z to 21:44:35Z. `gh api` runner busy flags were false for all three runners, `pgrep` found no `Runner.Worker`, and `gh run list` showed no in-progress or queued run in `blyzer/oh-my-pi` |

**Cause.** CI was not the cause, because no runner was busy in any guard or poll. The cause was
interactive use plus memory pressure. Several agent sessions were running on the Mac. At about
21:37Z Claude.app started, with Safari WebContent, Siri `embeddedspeech` and `heard` active, and
the load reached 107.

**Precondition for a valid run.** Schedule the run for a time when all of these hold for the whole
session:

- No interactive use of the Mac.
- No agent sessions and no other local builds, tests or indexers.
- No CI on any of the three runner services. Hold merges to `omp2`, and pushes or PR updates that
  touch a `.github/ci-paths.txt` path. Docs-only changes outside `docs/py/**` do not start the
  Mac job. The scheduled `p8-baseline` (`macos-15`) and `codeql` (`ubuntu-24.04`) workflows use
  hosted runners, and `package-macos` is manual.
- Memory pressure cleared, because the guard needs zero pageouts and swapouts over 10 s and a
  one-minute load of 1.0 or less. [INFERENCE] With 16.5-22.8 GiB already in swap, that probably
  needs memory-heavy apps closed or a restart first.

Repeat the setup above in a fresh worktree before the window opens, because building and warming
load the host. [INFERENCE] From earlier per-repetition wall times, the 18 repetitions and their
settle waits take about 2-3 hours.

**Not measured.** Relief remains unmeasured for all three recipes, and this record proposes no
threshold. Some quantities cannot be measured on this path whatever the host does. Warm remote wall
time cannot be judged, because the product path has no warm-state backend to compare against. The
`buildbuddy` REAPI profile requests no runner recycling and keeps the action cache off, so every
remote repetition is cold. FindMissing time has no span. The remote runner's actual freshness and
the probe overhead remain unverified.

