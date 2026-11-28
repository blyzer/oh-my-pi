# 61 Standalone CPython subprocess startup

> Part of the OMP2 gateway-capabilities research. Measurement for deliverable 8. Audited commit `85d219ae7f`.

## Command
Python `subprocess.run(argv)` timed with `time.perf_counter`, 30 runs per case, stdout and stderr to /dev/null, run sequentially from one script. Wall time of spawn plus exec plus interpreter init plus exit.

## Environment
```
arm64
Darwin 27.2.0
17179869184
10
23:00  up 3 days, 11:24, 1 user, load averages: 7.43 14.91 16.16
/opt/homebrew/bin/python3
Python 3.15.0
```
`uptime` immediately before each case:
```
23:00  up 3 days, 11:24, 1 user, load averages: 7.30 14.64 16.05
23:00  up 3 days, 11:24, 1 user, load averages: 8.39 14.74 16.08
23:01  up 3 days, 11:24, 1 user, load averages: 8.39 14.74 16.08
23:01  up 3 days, 11:24, 1 user, load averages: 8.39 14.74 16.08
```
The load average (7 to 16 on 10 cores) shows other work was running; numbers are inflated and noisy and are not a benchmark.

## Results (ms)
"Cold" is the first run of each case (the page cache was NOT purged, `purge` needs root, so this is cold only for the first touch of that binary in this script, not a true cold boot). Warm is the median of runs 2 to 30.

| case | first run | median runs 2-30 | median all 30 | min | max |
|---|---|---|---|---|---|
| python3 -c pass (homebrew 3.15.0) | 63.7 | 49.4 | 49.6 | 40.6 | 94.1 |
| python3 -I -S -c pass | 31.1 | 32.0 | 31.9 | 25.3 | 53.3 |
| vendor python3.14t -c pass (release pbs build, same CPython as embedded) | 627.0 | 27.3 | 27.4 | 23.7 | 627.0 |
| vendor python3.14t -I -S -c pass | 22.6 | 36.6 | 36.5 | 22.6 | 56.1 |

## Raw per-run values (ms)
```
python3 -c pass (homebrew 3.15.0): 63.7 62.2 47.1 56.4 56.5 94.1 65.2 67.9 48.0 49.4 42.2 44.2 43.0 41.6 44.8 40.8 64.9 48.3 54.8 49.7 50.1 57.5 43.9 40.6 50.2 47.8 44.7 47.5 59.9 73.6
python3 -I -S -c pass: 31.1 27.5 30.8 30.3 31.7 40.5 47.8 46.0 33.4 46.0 38.9 33.5 32.4 32.9 37.4 41.5 27.4 27.4 28.4 28.6 25.3 32.0 33.1 29.9 26.5 26.1 33.1 30.9 53.3 30.4
vendor python3.14t -c pass (release pbs build, same CPython as embedded): 627.0 33.5 25.5 27.7 24.5 26.6 25.6 39.1 34.6 30.5 24.8 31.8 24.3 23.7 46.3 38.5 29.3 27.3 35.5 25.7 23.9 25.0 25.5 23.7 29.4 34.7 32.8 24.8 27.4 26.6
vendor python3.14t -I -S -c pass: 22.6 29.7 26.0 45.5 44.7 35.5 37.6 38.1 30.3 23.5 23.5 23.2 30.5 25.0 24.5 27.9 37.7 39.2 35.1 41.1 35.1 50.1 36.5 37.7 49.7 39.8 56.1 39.2 51.3 36.6
```

## What this shows and does not show
Shows the cost floor of starting a standalone CPython process on this laptop, including the exact vendored free-threaded 3.14 build that `omp-py` links statically. Does NOT show the startup of the embedded interpreter inside `omp __omp-eval-child` or `__omp-ext-host`: no `omp` binary was built (`target/debug` has no `omp`), and the embedded boot also installs the frozen table, the `_omp` module and the omp bootstrap. Embedded child startup is `not measured` (plan in deliverable 8, PY-02).
