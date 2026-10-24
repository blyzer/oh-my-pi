# Endpoint-scanner filesystem cost on the macOS CI host

Measured 2026-10-23 on `Envers-MacBook-Pro`, the self-hosted runner that owns
every macOS CI job (`vars.MACOS_RUNNER` = `["self-hosted","macOS","ARM64"]`).
No configuration was changed; this records what the host costs today and what
an exclusion would be worth. Acting on it is the owner's call.

## Why this was measured

A P7 acceptance proof failed deterministically
(`tsp_fallback_paths_never_open_a_surface`, PR #380) because `omp chat` had
not written its first PTY byte inside a six-second window. A `sample` of the
live process showed the main thread parked in `unlink`, closing a SQLite
write-ahead log, for 1097 of 1136 samples. The code defect that amplified it
is fixed in PRs #381 and #382; the host cost underneath it is this note.

## Numbers

Two observations, both from this machine, hours apart.

**During the failure**, with the machine saturated:

| Operation | min | median | max |
|---|---|---|---|
| create a 4 KB file | 2.0 ms | 140 ms | 1233 ms |
| unlink | 0.5 ms | 15 ms | 98 ms |
| SQLite open + WAL + close | 328 ms | 895 ms | 6987 ms |

**Later, while CI was building** (rustc at 219% CPU, the Sophos system
extension at 143%, `SophosCryptoGuard` at 79%, `SophosEventMonitor` at 51%):

| Directory | create min/median/max | unlink min/median/max |
|---|---|---|
| `target/` | 0.38 / 0.69 / 3.31 ms | 0.51 / 1.04 / 4.35 ms |
| system temp (test scratch) | 0.30 / 0.48 / 1.31 ms | 0.39 / 0.74 / 2.87 ms |
| home | 0.31 / 0.47 / 2.28 ms | 0.41 / 0.77 / 2.99 ms |

For scale, an unencumbered APFS volume on Apple silicon does these in roughly
0.02-0.05 ms. The good case here is therefore about 15-25x slower than
typical, and the bad case about 3000x.

## What that costs a build

`target/` holds **141,975 files**. Creating and deleting each once costs:

| Per-file cost | Pure filesystem overhead |
|---|---|
| today's measured 0.69 ms + 1.04 ms | ~4.1 minutes |
| the failure-window 140 ms + 15 ms | ~367 minutes |

The second row is an upper bound rather than a prediction: an incremental
build does not rewrite every file, and not every file is created and then
deleted. It is still the right order of magnitude for why a 180-minute CI
timeout and 20-second in-test deadlines both blew during that window, and why
the same suite that passes in minutes can appear to hang.

## What would help, and what I did not do

The scanner hooks every file create and delete, so the directories worth
excluding are the ones builds and tests churn:

- `~/repositories/oh-my-pi/target` and any `target/` under a worktree
- `~/actions-runner-omp/_work`
- the system temp directory, which every `tempfile::tempdir()` test scratch
  and every SQLite WAL in those tests lives in
- `~/.cargo/registry` and `~/.rustup`

I did not measure the gain from excluding them, because that requires turning
the scanner off or adding exclusions, and neither is mine to do. The expected
saving is bounded above by the table in the previous section: on a clean build
it is minutes today, and hours in the pathological state. **[INFERENCE]** that
the Sophos extension is the cause rests on the CPU attribution above and on
the shape of the stall (kernel time inside `unlink`), not on an A/B with it
disabled.

## Related

- PR #380 removed the fixed-window assertion the slowdown broke.
- PRs #381 and #382 removed the per-operation SQLite connection churn that
  turned each slow `unlink` into several per store operation.
- `docs/audits/remote-verification-trial.md` covers moving heavy verification
  off this host entirely, which is the other way to avoid the cost.
