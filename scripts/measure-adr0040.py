#!/usr/bin/env python3
import argparse
import importlib.util
import json
import os
import re
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "quiet_host", Path(__file__).with_name("quiet-host.py")
)
assert spec is not None and spec.loader is not None
quiet = importlib.util.module_from_spec(spec)
spec.loader.exec_module(quiet)

RECIPES = [
    {
        "name": "just test-pkg omp-envd",
        "local": ["just", "test-pkg", "omp-envd"],
        "workflow": "g1",
    },
    {
        "name": "just test-pkg omp-core",
        "local": ["just", "test-pkg", "omp-core"],
        "workflow": "g1-core",
    },
    {"name": "just clippy", "local": ["just", "clippy"], "workflow": "g1-clippy"},
]
REPETITIONS = 3
DURATION_RE = re.compile(r"([0-9.]+)(ns|µs|us|ms|s|m|h)")
TIME_RE = re.compile(r"([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys")


def write_record(out, record):
    out.write(json.dumps(record, ensure_ascii=False) + "\n")
    out.flush()


def cache_state(root, omp_bin):
    values = {}
    for key in (
        "sv_exec_remote_cache",
        "sv_exec_remote_profile",
        "sv_exec_remote_image",
        "sv_exec_remote_endpoint",
    ):
        result = subprocess.run(
            [omp_bin, "config", "get", key],
            cwd=root,
            text=True,
            capture_output=True,
            check=False,
        )
        if result.returncode:
            raise RuntimeError(f"cannot read {key}: {result.stderr.strip()}")
        values[key] = result.stdout.strip()
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    return {
        "action_cache": values["sv_exec_remote_cache"],
        "profile": values["sv_exec_remote_profile"],
        "image": values["sv_exec_remote_image"],
        "endpoint": values["sv_exec_remote_endpoint"],
        "cargo_home": str(cargo_home),
        "registry_dirs_present": {
            name: (cargo_home / "registry" / name).exists()
            for name in ("cache", "src", "index")
        },
        "cargo_target_dir": os.environ.get("CARGO_TARGET_DIR"),
        "remote_runner": "REAPI BuildBuddy profile requests no runner recycling; actual runner state is not independently verified",
    }


def fetch_crates(root, *, synthetic=False):
    command = ["/usr/bin/true"] if synthetic else ["cargo", "fetch", "--locked"]
    result = subprocess.run(
        command, cwd=root, text=True, capture_output=True, check=False
    )
    return {
        "type": "fetch",
        "command": command,
        "exit": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "timed": False,
        "synthetic": synthetic,
    }


def disk_size(root):
    target = root / "target"
    if not target.exists():
        return {"exists": False, "bytes": 0}
    output = subprocess.run(
        ["du", "-sk", str(target.resolve())], text=True, capture_output=True, check=True
    ).stdout
    return {"exists": True, "bytes": int(output.split()[0]) * 1024}


def value_or_missing(value, failed, reason):
    if value is not None:
        return {"status": "captured", "value": value}
    return {"status": "run_failed" if failed else "not_captured", "reason": reason}


def duration_ms(text):
    match = DURATION_RE.fullmatch(text)
    if not match:
        return None
    return (
        float(match.group(1))
        * {
            "ns": 0.000001,
            "µs": 0.001,
            "us": 0.001,
            "ms": 1,
            "s": 1000,
            "m": 60000,
            "h": 3600000,
        }[match.group(2)]
    )


def span_ms(text, name):
    for line in text.splitlines():
        if name not in line or "close" not in line:
            continue
        busy = re.search(r"time\.busy=([^\s]+)", line)
        idle = re.search(r"time\.idle=([^\s]+)", line)
        if busy and idle:
            b, i = duration_ms(busy.group(1)), duration_ms(idle.group(1))
            if b is not None and i is not None:
                return round(b + i, 6)
    return None


def trace_int(text, key):
    match = re.search(re.escape(key) + r"=(?:Some\()?(-?\d+)", text)
    return int(match.group(1)) if match else None


def remote_metrics(text, *, failed, synthetic):
    keys = {
        "capture_ms": ("span", "omp.exec.capture"),
        "upload_inclusive_ms": ("span", "omp.exec.upload"),
        "download_ms": ("span", "omp.exec.download"),
        "upload_bytes": ("int", "omp.exec.upload.bytes"),
        "queue_ms": ("int", "omp.exec.worker.queue_ms"),
        "input_fetch_ms": ("int", "omp.exec.worker.input_fetch_ms"),
        "execution_ms": ("int", "omp.exec.worker.exec_ms"),
        "output_upload_ms": ("int", "omp.exec.worker.output_upload_ms"),
        "cas_hits": ("int", "omp.exec.cas.hits"),
        "cas_misses": ("int", "omp.exec.cas.misses"),
    }
    if synthetic:
        return {key: {"status": "synthetic"} for key in (*keys, "find_missing_ms")}
    metrics = {}
    for key, (kind, source) in keys.items():
        value = span_ms(text, source) if kind == "span" else trace_int(text, source)
        metrics[key] = value_or_missing(value, failed, f"{source} not present in trace")
    metrics["find_missing_ms"] = {
        "status": "not_captured",
        "reason": "REAPI trace has no FindMissing span",
    }
    return metrics


def time_metrics(stderr, failed, synthetic):
    if synthetic:
        return {"status": "synthetic"}
    times = list(TIME_RE.finditer(stderr))
    rss_values = list(
        re.finditer(r"^\s*(\d+)\s+maximum resident set size", stderr, re.MULTILINE)
    )
    if not times or not rss_values:
        return {
            "status": "run_failed" if failed else "not_captured",
            "reason": "/usr/bin/time -l output incomplete",
        }
    match, rss = times[-1], rss_values[-1]
    return {
        "status": "captured",
        "wall_s": float(match.group(1)),
        "user_s": float(match.group(2)),
        "sys_s": float(match.group(3)),
        "cpu_s": float(match.group(2)) + float(match.group(3)),
        "max_single_descendant_rss_bytes": int(rss.group(1)),
    }


def synthetic_guard():
    return {
        "type": "guard",
        "verdict": "synthetic",
        "synthetic": True,
        "snapshot": {"status": "synthetic"},
        "refusals": [],
    }


def run_one(out, recipe, arm, repetition, *, root, omp_bin, dry_run, guard):
    before = {"status": "synthetic"} if dry_run else disk_size(root)
    if dry_run:
        command = ["/usr/bin/true"]
    elif arm == "local":
        command = recipe["local"]
    else:
        command = [
            omp_bin,
            "adw",
            "run",
            recipe["workflow"],
            "--project",
            str(root),
            "--exec-policy",
            "remote-only",
            "--max-attempts",
            "1",
        ]
    label = f"{recipe['name']} / {arm} / {repetition}"
    write_record(
        out,
        {
            "type": "run_context",
            "label": label,
            "recipe": recipe["name"],
            "arm": arm,
            "repetition": repetition,
            "target_before": before,
            "command": command,
            "synthetic": dry_run,
        },
    )
    env = dict(os.environ)
    if arm == "remote" and not dry_run:
        env.update(OMP_TIMING="1", OMP_LOG_STDERR="info")
    with (
        tempfile.TemporaryFile(mode="w+t", encoding="utf-8") as stdout,
        tempfile.TemporaryFile(mode="w+t", encoding="utf-8") as stderr,
    ):
        end = quiet.run_recipe(
            ["/usr/bin/time", "-l", *command],
            "",
            label,
            1.0,
            guard,
            output=out,
            stdout=stdout,
            stderr=stderr,
            env=env,
            cwd=root,
            project_root=str(root) if arm == "remote" and not dry_run else None,
            echo=False,
        )
        stdout.seek(0)
        stderr.seek(0)
        stdout_text, stderr_text = stdout.read(), stderr.read()
    after = {"status": "synthetic"} if dry_run else disk_size(root)
    failed = end["exit"] != 0
    timing = time_metrics(stderr_text, failed, dry_run)
    accounting = None
    if arm == "remote":
        if dry_run:
            accounting = {
                "status": "synthetic",
                "attachment": "synthetic",
                "sharing": "synthetic",
                "ownership": {"status": "synthetic"},
            }
        elif (
            end["envd_before"]["status"] == "captured"
            and end["envd_after"]["status"] == "captured"
        ):
            accounting = quiet.envd_accounting(
                end["envd_before"]["processes"],
                end["envd_after"]["processes"],
                end["envd_observed"],
                timing.get("cpu_s"),
                end.get("envd_ownership"),
            )
        else:
            accounting = {
                "status": "unresolved",
                "attachment": "unresolved",
                "ownership": end.get("envd_ownership"),
                "sharing": "unresolved",
                "reason": "envd process probe failed before or after the run",
            }
    return {
        "type": "repetition",
        "recipe": recipe["name"],
        "arm": arm,
        "repetition": repetition,
        "status": "failed" if failed else "synthetic" if dry_run else "passed",
        "exit": end["exit"],
        "command": command,
        "target_before": before,
        "target_after": after,
        "target_delta_bytes": None
        if dry_run
        else int(after["bytes"]) - int(before["bytes"]),
        "time": timing,
        "sampler": end,
        "client_accounting": accounting,
        "conditions": {
            "envd_before": end.get("envd_before"),
            "envd_after": end.get("envd_after"),
            "sharing": (accounting or {}).get("sharing", "not_applicable"),
        },
        "content": {
            "in_tree_time": timing,
            "in_tree_peak_rss_mib": end.get("tree_peak_rss_mib"),
            "serving_envd": accounting,
            "rss_bounds": {
                "lower_mib": end.get("tree_without_envd_peak_rss_mib"),
                "upper_mib": end.get("tree_plus_envd_peak_rss_mib"),
                "status": "synthetic"
                if dry_run
                else "sampled_conditional"
                if end.get("tree_plus_envd_peak_rss_mib") is not None
                else "unresolved",
                "reason": "One-second RSS samples and lsof visibility may miss short-lived peaks or peers.",
            },
        },
        "remote_metrics": remote_metrics(stderr_text, failed=failed, synthetic=dry_run)
        if arm == "remote"
        else None,
        "stdout": stdout_text,
        "stderr": stderr_text,
    }


def summary(repetitions, synthetic):
    lines = [
        "SYNTHETIC DRY RUN; NOT A MEASUREMENT" if synthetic else "ADR 0040 G1 summary"
    ]

    def metric(record, key):
        if key == "tree_plus_envd_cpu_s":
            return (
                (record.get("client_accounting") or {}).get("cpu_bounds") or {}
            ).get("upper_s")
        if key in (
            "tree_peak_rss_mib",
            "tree_without_envd_peak_rss_mib",
            "tree_plus_envd_peak_rss_mib",
        ):
            return record.get("sampler", {}).get(key)
        source = record.get("sampler", {}) if synthetic else record.get("time", {})
        return (
            source.get(key)
            if source.get("status", "captured") in ("captured", "synthetic")
            else None
        )

    for recipe in RECIPES:
        lines += [
            "",
            f"### {recipe['name']}",
            "| Metric | Local | Remote per repetition | Ratio |",
            "|---|---|---|---|",
        ]
        groups = {
            arm: [
                r
                for r in repetitions
                if r["recipe"] == recipe["name"] and r["arm"] == arm
            ]
            for arm in ("local", "remote")
        }
        rows = (
            ("Wall", "wall_s", "s"),
            ("CPU (user+sys, in-tree)", "cpu_s", "s"),
            ("CPU (tree + whole envd, conditional upper)", "tree_plus_envd_cpu_s", "s"),
            ("Peak memory (process-tree RSS)", "tree_peak_rss_mib", "MiB"),
            (
                "Peak memory (tree excluding envd, sampled lower)",
                "tree_without_envd_peak_rss_mib",
                "MiB",
            ),
            (
                "Peak memory (tree + whole envd, sampled upper)",
                "tree_plus_envd_peak_rss_mib",
                "MiB",
            ),
        )
        for title, key, unit in rows:
            local_values = [
                metric(record, key)
                for record in groups["local"]
                if record["status"] in ("passed", "synthetic")
            ]
            local_values = [value for value in local_values if value is not None]
            local = (
                f"{statistics.mean(local_values):.2f} {unit}"
                if local_values
                else "not available"
            )
            remote_values = []
            for record in groups["remote"]:
                value = (
                    metric(record, key)
                    if record["status"] in ("passed", "synthetic")
                    else None
                )
                account = record.get("client_accounting") or {}
                boundary = f"{account.get('attachment', 'unresolved')}, {account.get('sharing', 'unresolved')}"
                shown = f"{value:.2f} {unit}" if value is not None else "not available"
                remote_values.append(
                    f"rep {record['repetition']}: {shown} ({boundary})"
                )
            remote = "; ".join(remote_values) if remote_values else "not available"
            lines.append(f"| {title} | {local} | {remote} | withheld |")
        for arm, group in groups.items():
            failed = sum(r["status"] == "failed" for r in group)
            lines.append(
                f"{arm}: {len(group)} recorded, {failed} failed; failed repetitions excluded, never retried."
            )
        lines.append(
            "FindMissing: synthetic; no remote action ran."
            if synthetic
            else "FindMissing: not captured; the REAPI trace has no FindMissing span. Upload time includes FindMissing."
        )
    lines.append(
        "Client ratio verdict withheld: envd socket attribution, shared work and CPU overlap remain unresolved."
    )
    lines.append(
        "Warm remote wall-time criterion: not measurable; this path requests no recycled runner."
    )
    lines.append(
        "The time -l maximum RSS is one descendant, not the process-tree peak."
    )
    return "\n".join(lines)


def run_session(path, *, root, omp_bin, dry_run):
    repetitions = []
    with open(path, "x", encoding="utf-8") as out:
        write_record(
            out,
            {
                "type": "session",
                "schema": 1,
                "synthetic": dry_run,
                "project": str(root),
                "started_unix_s": time.time(),
                "recipes": [r["name"] for r in RECIPES],
                "repetitions_per_arm": REPETITIONS,
                "boundary": {
                    "definition": "role, not process ancestry",
                    "content": [
                        "command descendants",
                        "socket-proven serving envd owner",
                    ],
                    "conditions": [
                        "host load",
                        "paging",
                        "runner jobs",
                        "other processes and envd peers",
                    ],
                    "sharing": "whole shared envd gives a conditional upper bound; no client split is invented",
                },
            },
        )
        status, reason, code = "complete", "", 0
        try:

            def check_guard():
                guard = (
                    synthetic_guard()
                    if dry_run
                    else quiet.guard(quiet.DEFAULT_MAX_LOAD1)
                )
                guard.setdefault("synthetic", dry_run)
                write_record(out, guard)
                if not dry_run and guard["verdict"] == "refuse":
                    for refusal in guard.get("refusals", []):
                        print(f"REFUSED {quiet.describe(refusal)}", file=sys.stderr)
                return guard

            initial = check_guard()
            if initial["verdict"] != ("synthetic" if dry_run else "pass"):
                raise RuntimeError("pre-flight guard refused")
            state = {"status": "synthetic"} if dry_run else cache_state(root, omp_bin)
            write_record(out, {"type": "cache_state", **state})
            if not dry_run and (
                state["action_cache"] != "off" or state["profile"] != "buildbuddy"
            ):
                raise RuntimeError(
                    "remote cache must already be off and profile must already be buildbuddy; settings were not changed"
                )
            fetched = fetch_crates(root, synthetic=dry_run)
            write_record(out, fetched)
            if fetched["exit"]:
                raise RuntimeError("untimed cargo fetch failed")
            for recipe in RECIPES:
                for repetition in range(1, REPETITIONS + 1):
                    for arm in ("local", "remote"):
                        guard = check_guard()
                        if guard["verdict"] != ("synthetic" if dry_run else "pass"):
                            raise RuntimeError("between-repetition guard refused")
                        record = run_one(
                            out,
                            recipe,
                            arm,
                            repetition,
                            root=root,
                            omp_bin=omp_bin,
                            dry_run=dry_run,
                            guard=guard,
                        )
                        write_record(out, record)
                        repetitions.append(record)
            if any(record["status"] == "failed" for record in repetitions):
                status, reason, code = (
                    "complete_with_failures",
                    "one or more repetitions failed",
                    1,
                )
        except (
            OSError,
            RuntimeError,
            ValueError,
            subprocess.SubprocessError,
            KeyboardInterrupt,
        ) as error:
            status, reason, code = "aborted", str(error) or type(error).__name__, 2
        rendered = summary(repetitions, dry_run)
        write_record(
            out, {"type": "summary", "synthetic": dry_run, "markdown": rendered}
        )
        write_record(
            out,
            {
                "type": "session_end",
                "status": status,
                "reason": reason,
                "synthetic": dry_run,
            },
        )
    print(rendered)
    print(f"Session {status}: {reason}" if reason else f"Session {status}.")
    print(f"Result: {path}")
    return code


def main():
    parser = argparse.ArgumentParser(
        description="Record interleaved ADR 0040 local and cold-remote repetitions."
    )
    parser.add_argument("--out", required=True, type=Path, help="new JSONL result file")
    parser.add_argument("--omp", default="target/debug/omp", help="prebuilt omp binary")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="exercise the path with synthetic guards and /usr/bin/true, not a measurement",
    )
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    if args.out.resolve().is_relative_to(root):
        parser.error(
            "--out must be outside the checkout to avoid capturing the growing result"
        )
    binary = Path(args.omp)
    omp_bin = str(binary if binary.is_absolute() else (root / binary).resolve())

    def terminate(_signum, _frame):
        raise KeyboardInterrupt("terminated by signal")

    previous = signal.signal(signal.SIGTERM, terminate)
    try:
        return run_session(args.out, root=root, omp_bin=omp_bin, dry_run=args.dry_run)
    except FileExistsError:
        print(f"error: output already exists: {args.out}", file=sys.stderr)
        return 2
    except OSError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    finally:
        signal.signal(signal.SIGTERM, previous)


if __name__ == "__main__":
    sys.exit(main())
