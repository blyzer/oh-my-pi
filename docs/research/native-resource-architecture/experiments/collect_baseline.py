#!/usr/bin/env python3
"""Bounded research collection, no production writes or paid provider calls.

Source the task toolchain first; run from the OMP2 checkout. Outputs only this
experiment's raw files. Instrumented synthetic timings are not OMP2 timings.
"""
import collections
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import tempfile
import time
import tomllib

ROOT = Path(__file__).resolve().parents[4]
OUT = Path(__file__).resolve().parent


def command(args, timeout=60):
    start = time.monotonic_ns()
    try:
        result = subprocess.run(args, cwd=ROOT, text=True, capture_output=True, timeout=timeout)
        return {"argv": args, "exit": result.returncode, "elapsed_ns": time.monotonic_ns()-start,
                "stdout": result.stdout, "stderr": result.stderr[-8192:]}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"argv": args, "exit": None, "elapsed_ns": time.monotonic_ns()-start,
                "error_type": type(error).__name__}


def main():
    pin = command(["git", "rev-parse", "HEAD"])["stdout"].strip()
    metadata = command(["cargo", "metadata", "--offline", "--locked", "--no-deps", "--format-version", "1"])
    inventory = {}
    if metadata["exit"] == 0:
        data = json.loads(metadata.pop("stdout"))
        inventory = {"workspace_members": len(data["workspace_members"]),
                     "packages": [{"name": p["name"], "targets": [{"name": t["name"], "kind": t["kind"]} for t in p["targets"]],
                                   "features": p["features"], "dependencies": len(p["dependencies"])} for p in data["packages"]]}
        metadata["stdout_note"] = "Parsed package/target/features recorded in inventory; no dependency resolution implied."
    lock = tomllib.loads((ROOT/"Cargo.lock").read_text())["package"]
    versions = collections.defaultdict(list)
    for package in lock:
        versions[package["name"]].append(package["version"])
    inventory["lock_packages"] = len(lock)
    inventory["duplicate_name_versions"] = {k: v for k, v in versions.items() if len(v) > 1}
    environment = {"pin": pin, "platform": platform.platform(), "machine": platform.machine(),
                   "cpu_count_visible": os.cpu_count(), "meminfo": Path("/proc/meminfo").read_text(),
                   "cgroup_memory_max": Path("/sys/fs/cgroup/memory.max").read_text().strip(),
                   "cgroup_cpu_max": Path("/sys/fs/cgroup/cpu.max").read_text().strip(),
                   "tools": {name: shutil.which(name) for name in ["cargo", "rustc", "just", "cargo-nextest", "perf", "heaptrack", "valgrind", "cargo-bloat", "cargo-llvm-lines", "vmmap", "footprint", "sample"]}}
    calls = [metadata, command(["rustc", "-Vv"]), command(["cargo", "-V"])]
    tree = command(["cargo", "tree", "--offline", "--locked", "-d"])
    if tree["exit"] == 0:
        tree["stdout_sha256"] = hashlib.sha256(tree["stdout"].encode()).hexdigest()
        tree["stdout_lines"] = len(tree["stdout"].splitlines())
        tree["stdout"] = tree["stdout"][:8192]
        tree["stdout_note"] = "Excerpt only; command can regenerate full tree. Lock duplicate inventory is complete."
    calls.append(tree)
    calls.append(command(["env", "CARGO_TARGET_DIR=/tmp/omp-native-core-target", "just", "test-pkg", "omp-core"], timeout=120))
    calls.append(command(["env", "CARGO_TARGET_DIR=/tmp/omp-native-core-target", "just", "test-pkg", "omp-journal"], timeout=120))
    compile_result = command(["rustc", "-O", "--edition=2024", str(OUT/"ownership_probe.rs"), "-o", "/tmp/omp-ownership-probe"])
    calls.append(compile_result)
    summaries = []
    if compile_result["exit"] == 0:
        for nodes in (10_000, 100_000):
            result = command(["/tmp/omp-ownership-probe", str(nodes)])
            calls.append(result)
            if result["exit"] == 0:
                rows = [json.loads(line) for line in result["stdout"].splitlines()]
                for method in ("deep_clone", "arc_clone"):
                    samples = [r for r in rows if r["kind"] == "sample" and r["method"] == method]
                    summaries.append({"nodes": nodes, "method": method, "n": len(samples),
                                      "median_elapsed_ns": statistics.median(r["elapsed_ns"] for r in samples),
                                      "min_elapsed_ns": min(r["elapsed_ns"] for r in samples),
                                      "max_elapsed_ns": max(r["elapsed_ns"] for r in samples),
                                      "allocation_calls": sorted(set(r["allocation_calls"] for r in samples)),
                                      "requested_bytes": sorted(set(r["requested_bytes"] for r in samples))})
    target = Path("/tmp/omp-native-core-target/debug/build")
    libraries = list(target.glob("omp-core/*/out/libomp_core-*.rlib"))
    if len(libraries) == 1:
        link = ["rustc", "-O", "--edition=2024", str(OUT/"core_str_probe.rs"), "--extern", "omp_core="+str(libraries[0]), "-o", "/tmp/omp-core-str-probe"]
        # The workspace uses split metadata: rustc needs both explicit artifacts.
        link += ["--extern", "omp_core="+str(libraries[0].with_suffix(".rmeta"))]
        for directory in sorted(target.glob("*/*/out")):
            link += ["-L", "dependency="+str(directory)]
        built = command(link)
        calls.append(built)
        if built["exit"] == 0:
            for nodes in (10_000, 100_000):
                result = command(["/tmp/omp-core-str-probe", str(nodes)])
                calls.append(result)
                if result["exit"] == 0:
                    samples = [json.loads(line) for line in result["stdout"].splitlines()]
                    summaries.append({"nodes": nodes, "method": "omp_core_str_graph_clone", "n": len(samples),
                                      "median_elapsed_ns": statistics.median(r["elapsed_ns"] for r in samples),
                                      "min_elapsed_ns": min(r["elapsed_ns"] for r in samples),
                                      "max_elapsed_ns": max(r["elapsed_ns"] for r in samples),
                                      "allocation_calls": sorted(set(r["allocation_calls"] for r in samples)),
                                      "requested_bytes": sorted(set(r["requested_bytes"] for r in samples))})
    journal = list(target.glob("omp-journal/*/out/libomp_journal-*.rlib"))
    blob_summary = []
    if len(journal) == 1:
        link = ["rustc", "-O", "--edition=2024", str(OUT/"blob_range_probe.rs"), "--extern", "omp_journal="+str(journal[0]), "-o", "/tmp/omp-blob-range-probe"]
        link += ["--extern", "omp_journal="+str(journal[0].with_suffix(".rmeta"))]
        for directory in sorted(target.glob("*/*/out")):
            link += ["-L", "dependency="+str(directory)]
        built = command(link)
        calls.append(built)
        if built["exit"] == 0:
            for size in (1_048_576, 10_485_760, 52_428_800):
                with tempfile.TemporaryDirectory(prefix="omp-blob-probe-") as temporary:
                    result = command(["/tmp/omp-blob-range-probe", str(size), temporary])
                calls.append(result)
                if result["exit"] == 0:
                    rows = [json.loads(line) for line in result["stdout"].splitlines()]
                    for method in ("get_then_slice", "open_range_then_read"):
                        samples = [row for row in rows if row["method"] == method]
                        blob_summary.append({"blob_bytes": size, "range_bytes": 4096, "method": method, "n": len(samples),
                                             "median_elapsed_ns": statistics.median(row["elapsed_ns"] for row in samples),
                                             "allocation_calls": sorted(set(row["allocation_calls"] for row in samples)),
                                             "requested_bytes": sorted(set(row["requested_bytes"] for row in samples)),
                                             "retained_requested_bytes_during_return": sorted(set(row["retained_requested_bytes_during_return"] for row in samples))})
    result = {"environment": environment, "inventory": inventory, "commands": calls,
              "ownership_probe_summary": summaries,
              "actual_blob_api_summary": blob_summary,
              "limitations": ["Synthetic graph, not an implemented OMP2 IR or runtime baseline.",
                              "System allocation counters instrument every allocation; timing overhead is included.",
                              "Arc aliases immutable state; not a substitute for an independent mutable deep clone.",
                              "No OS cache purge; no production allocation profile, macOS measurement or paid inference."]}
    (OUT/"baseline-results-final.json").write_text(json.dumps(result, indent=2, sort_keys=True)+"\n")
    print(json.dumps({"pin": pin, "workspace_members": inventory.get("workspace_members"),
                      "lock_packages": len(lock), "duplicate_names": len(inventory["duplicate_name_versions"]),
                      "summary": summaries}, indent=2))


if __name__ == "__main__":
    main()
