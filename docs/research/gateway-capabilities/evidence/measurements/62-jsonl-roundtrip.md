# 62 JSON-lines round trip to a persistent child Python

> Part of the OMP2 gateway-capabilities research. Measurement for deliverable 8. Audited commit `85d219ae7f`.

## Command
A Python 3.x script spawns a persistent child (`python -u -c <echo loop>`), sends 1000 newline-delimited JSON requests over stdin one at a time, waits for each JSON reply on stdout (strictly sequential, no pipelining), and times each round trip with `time.perf_counter`. Framing mirrors the production eval protocol (one JSON object per line, `crates/envd/src/eval/process.rs:1804 write_frame`) but the child is a trivial echo, not `external_runner.py`. One run of 1000 calls per interpreter (1 run, not repeated).

Child source:
```
import sys,json
for l in sys.stdin:
    r=json.loads(l)
    sys.stdout.write(json.dumps({'id':r['id'],'ok':True,'echo':r['args']},separators=(',',':'))+'\n'); sys.stdout.flush()
```

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
`uptime` before: `23:01  up 3 days, 11:24, 1 user, load averages: 10.49 14.89 16.10`. Machine was loaded (see load averages), so absolute numbers are inflated.

## Results
| interpreter | first call incl. warm-up (ms) | total for 1000 calls (ms) | median round trip (us) | p95 (us) | max (us) |
|---|---|---|---|---|---|
| homebrew python3 3.15.0 | 49.0 | 17.3 | 14 | 19 | 195 |
| vendor python3.14t | 130.5 | 20.0 | 17 | 22 | 162 |

## What this shows and does not show
Shows the floor of pipe plus JSON encode/decode cost per call to a persistent standalone CPython child, parent in Python. It does NOT include `external_runner.py` dispatch, the authenticated bridge, Rust-side serde cost, the sandbox, or any PyO3 callback. It says nothing about the embedded child; the in-process PyO3 callback overhead is `not measured` (plan in PY-03).

Back to the [index](../../README.md).
