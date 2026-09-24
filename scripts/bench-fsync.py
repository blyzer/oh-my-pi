#!/usr/bin/env python3
"""Cost of one small durable append on this volume: no sync, fsync, F_FULLFSYNC.

Throwaway experiment for the P8 gross_regression investigation. Each round
appends one ~200-byte line (the size of a journal stream entry) and then
syncs the way the variant says, like omp_journal::Journal::append does.
"""
import fcntl, os, statistics, sys, tempfile, time

ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 300
LINE = b"x" * 200 + b"\n"

def run(name, sync):
    with tempfile.TemporaryDirectory(dir=os.environ.get("RUNNER_TEMP")) as d:
        fd = os.open(os.path.join(d, "j.oms"), os.O_CREAT | os.O_WRONLY | os.O_APPEND, 0o644)
        samples = []
        for _ in range(ROUNDS):
            t = time.perf_counter_ns()
            os.write(fd, LINE)
            sync(fd)
            samples.append(time.perf_counter_ns() - t)
        os.close(fd)
    samples.sort()
    p50 = samples[len(samples) // 2] / 1e6
    p95 = samples[int(len(samples) * 0.95)] / 1e6
    mean = statistics.fmean(samples) / 1e6
    print(f"{name:12s} rounds={ROUNDS} mean={mean:.3f} ms p50={p50:.3f} ms p95={p95:.3f} ms")

run("none", lambda fd: None)
run("fsync", os.fsync)
if hasattr(fcntl, "F_FULLFSYNC"):
    run("F_FULLFSYNC", lambda fd: fcntl.fcntl(fd, fcntl.F_FULLFSYNC))
