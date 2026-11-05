#!/usr/bin/env python3
import argparse
import contextlib
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time

RUNNER_PREFIX = "actions.runner."
BASELINE_SECONDS = 10.0
MAX_PAGING_DELTA = 0
EXPECTED_RUNNERS = {
    "actions.runner.blyzer-oh-my-pi.Envers-MacBook-Pro",
    "actions.runner.vostroRD.Envers-MacBook-Pro",
    "actions.runner.vostroRD.Envers-MacBook-Pro-2",
}
DEFAULT_MAX_LOAD1 = 1.0

SWAP_RE = re.compile(r"total = ([\d.]+)M\s+used = ([\d.]+)M\s+free = ([\d.]+)M")
PAGE_SIZE_RE = re.compile(r"page size of (\d+) bytes")
VM_COUNTER_RE = re.compile(r"^(Pageouts|Swapouts):\s+(\d+)\.", re.MULTILINE)

DESCRIPTION = (
    "ADR 0040 quiet-host harness. guard: pre-flight refusal check. run: guard, then the recipe "
    "with 1 Hz sampling. sample: the recipe with sampling and no guard (records are marked "
    "guarded: false). Output is JSONL, created exclusively: a start line with the guard verdict and "
    "snapshot, one sample line per interval, and an end line with the end snapshot. A probe that "
    "cannot be read refuses."
)


def probe(argv: list[str]) -> str:
    return subprocess.run(
        argv, capture_output=True, text=True, check=True, timeout=10
    ).stdout


def load_avg() -> list[float]:
    values = [
        float(v) for v in probe(["sysctl", "-n", "vm.loadavg"]).strip(" {}\n").split()
    ]
    if len(values) != 3:
        raise ValueError(f"vm.loadavg unparsable: {values}")
    return values


def swap_usage() -> dict:
    match = SWAP_RE.search(probe(["sysctl", "-n", "vm.swapusage"]))
    if match is None:
        raise ValueError("vm.swapusage unparsable")
    total, used, free = (float(group) for group in match.groups())
    return {"total_mib": total, "used_mib": used, "free_mib": free}


def vm_counters() -> dict:
    text = probe(["vm_stat"])
    page = PAGE_SIZE_RE.search(text)
    counters = dict(VM_COUNTER_RE.findall(text))
    if page is None or "Pageouts" not in counters or "Swapouts" not in counters:
        raise ValueError("vm_stat unparsable")
    return {
        "page_size": int(page.group(1)),
        "pageouts": int(counters["Pageouts"]),
        "swapouts": int(counters["Swapouts"]),
    }


def runner_services() -> dict:
    active, inactive = [], []
    lines = probe(["launchctl", "list"]).splitlines()
    if not lines or lines[0].split("\t") != ["PID", "Status", "Label"]:
        raise ValueError("launchctl list header unexpected")
    services = {}
    for line in lines[1:]:
        fields = line.split("\t")
        if len(fields) != 3:
            raise ValueError(f"launchctl list row unparsable: {line!r}")
        pid, _, label = fields
        if label.startswith(RUNNER_PREFIX):
            services[label] = None if pid == "-" else int(pid)
    processes = {}
    for line in probe(["ps", "-axo", "pid=,ppid=,comm="]).splitlines():
        pid, parent, command = line.strip().split(maxsplit=2)
        processes[int(pid)] = (int(parent), os.path.basename(command))
    children = {}
    for pid, (parent, _) in processes.items():
        children.setdefault(parent, []).append(pid)
    attributed_workers = set()
    for label in sorted(EXPECTED_RUNNERS | services.keys()):
        pid = services.get(label)
        if pid is None:
            inactive.append(label)
            continue
        if pid not in processes:
            raise ValueError(f"runner service {label} pid {pid} is absent from ps")
        descendants, stack = [], [pid]
        while stack:
            current = stack.pop()
            descendants.append(current)
            stack.extend(children.get(current, []))
        workers = [p for p in descendants if processes[p][1] == "Runner.Worker"]
        listeners = [p for p in descendants if processes[p][1] == "Runner.Listener"]
        attributed_workers.update(workers)
        state = "busy" if workers else "idle" if listeners else "unknown"
        active.append(
            {"label": label, "pid": pid, "state": state, "worker_pids": workers}
        )
    workers = {
        pid for pid, (_, command) in processes.items() if command == "Runner.Worker"
    }
    return {
        "active": active,
        "inactive": inactive,
        "unattributed_worker_pids": sorted(workers - attributed_workers),
    }


def process_table() -> dict:
    rows = {}
    output = probe(["ps", "-ww", "-axo", "pid=,ppid=,rss=,time=,lstart=,command="])
    for line in output.splitlines():
        parts = line.split(maxsplit=9)
        if len(parts) != 10:
            raise ValueError(f"ps process row unparsable: {line!r}")
        pid, parent, rss, cpu = parts[:4]
        elapsed = cpu.split(":")
        seconds = 0.0
        for part in elapsed:
            seconds = seconds * 60 + float(part)
        rows[int(pid)] = {
            "ppid": int(parent),
            "rss_kib": int(rss),
            "cpu_s": seconds,
            "started": " ".join(parts[4:9]),
            "command": parts[9],
        }
    return rows


def process_tree(rows: dict, roots) -> set:
    children = {}
    for pid, row in rows.items():
        children.setdefault(row["ppid"], []).append(pid)
    found, stack = set(), list(roots)
    while stack:
        pid = stack.pop()
        if pid in found or pid not in rows:
            continue
        found.add(pid)
        stack.extend(children.get(pid, []))
    return found


def envd_roots(rows: dict, project: str) -> list:
    marker = f" envd --root {project} --state-dir "
    return sorted(pid for pid, row in rows.items() if marker in row["command"])


def serving_envd(rows: dict, root: int, project: str) -> dict:
    roots = envd_roots(rows, project)
    tree = process_tree(rows, [root])
    clients = tree - process_tree(rows, roots)
    if not roots or not clients:
        return {
            "status": "unresolved",
            "reason": "no envd candidate or command process observed",
        }
    try:
        output = probe(["lsof", "-nP", "-U", "-F", "pfdn"])
    except Exception as error:
        return {"status": "unresolved", "reason": f"lsof failed: {error}"}
    entries, pid, entry = [], None, None
    for line in output.splitlines():
        if not line:
            continue
        if line[0] in ("p", "f"):
            if entry is not None:
                entries.append(entry)
                entry = None
            if line[0] == "p":
                pid = int(line[1:])
            else:
                entry = {"pid": pid}
        elif entry is not None and line[0] in ("d", "n"):
            entry[line[0]] = line[1:]
    if entry is not None:
        entries.append(entry)
    devices = {
        entry.get("d"): entry["pid"]
        for entry in entries
        if entry["pid"] in roots and entry.get("d")
    }
    pairs = {}
    for entry in entries:
        name = entry.get("n", "")
        if entry["pid"] not in clients or not name.startswith("->"):
            continue
        device = name[2:]
        owner = devices.get(device)
        if owner is not None:
            pairs.setdefault(owner, []).append(
                {
                    "client_pid": entry["pid"],
                    "client_socket": entry.get("d"),
                    "envd_socket": device,
                }
            )
    if len(pairs) != 1:
        return {
            "status": "unresolved",
            "reason": "no unique envd socket peer matched the command tree",
        }
    owner = next(iter(pairs))
    owner_devices = {device for device, pid in devices.items() if pid == owner}
    owner_tree = process_tree(rows, [owner])
    other_peers = sorted(
        {
            entry["pid"]
            for entry in entries
            if entry["pid"] not in tree | owner_tree
            and entry.get("n", "").startswith("->")
            and entry["n"][2:] in owner_devices
        }
    )
    return {
        "status": "identified",
        "pid": owner,
        "started": rows[owner]["started"],
        "method": "lsof_unix_socket_peer",
        "socket_pairs": pairs[owner],
        "sharing": "shared" if other_peers else "no_other_peer_observed",
        "other_peer_pids": other_peers,
        "coverage": "lsof-visible sockets at sample time; inaccessible or short-lived peers may be missed",
    }


def attribute_processes(
    rows: dict, root: int | None, project: str, *, owner=None
) -> dict:
    tree = process_tree(rows, [root])
    roots = envd_roots(rows, project)
    envd = []
    for pid in roots:
        daemon = {pid}
        envd.append(
            {
                "pid": pid,
                "ppid": rows[pid]["ppid"],
                "started": rows[pid]["started"],
                "rss_kib": rows[pid]["rss_kib"],
                "cpu_s": rows[pid]["cpu_s"],
                "in_tree": pid in tree,
                "tree_without_envd_rss_kib": sum(
                    rows[p]["rss_kib"] for p in tree - daemon
                )
                if tree
                else None,
                "combined_rss_kib": sum(rows[p]["rss_kib"] for p in tree | daemon)
                if tree
                else None,
            }
        )
    matched = next(
        (
            row
            for row in envd
            if owner
            and owner.get("status") == "identified"
            and row["pid"] == owner.get("pid")
            and row["started"] == owner.get("started")
        ),
        None,
    )
    return {
        "tree_rss_kib": sum(rows[pid]["rss_kib"] for pid in tree) if tree else None,
        "envd_rss_kib": matched["rss_kib"] if matched else None,
        "envd_cpu_s": matched["cpu_s"] if matched else None,
        "tree_without_envd_rss_kib": matched["tree_without_envd_rss_kib"]
        if matched
        else None,
        "combined_rss_kib": matched["combined_rss_kib"] if matched else None,
        "envd": envd,
    }


def envd_accounting(
    before: list,
    after: list,
    observed: list,
    tree_cpu_s: float | None,
    proof: dict | None,
) -> dict:
    def identity(row):
        return row["pid"], row["started"]

    prior = {identity(row): row for row in before}
    final = {identity(row): row for row in after}
    in_tree = {identity(row) for row in observed if row["in_tree"]}
    attachment, cpu = "unresolved", None
    sharing = proof.get("sharing", "unresolved") if proof else "unresolved"
    if proof and proof.get("status") == "identified":
        key = identity(proof)
        row = final.get(key)
        if row is not None and key in prior:
            attachment = "preexisting"
            cpu = row["cpu_s"] - prior[key]["cpu_s"]
        elif row is not None and key in in_tree:
            attachment = "spawned"
            cpu = row["cpu_s"]
    if cpu is not None and cpu < 0:
        cpu = None
        attachment = "unresolved"
    upper = (
        round(tree_cpu_s + cpu, 3)
        if tree_cpu_s is not None and cpu is not None
        else None
    )
    return {
        "attachment": attachment,
        "ownership": proof or {"status": "unresolved"},
        "sharing": sharing,
        "status": "bounded"
        if sharing == "shared" and upper is not None
        else "estimated"
        if upper is not None
        else "unresolved",
        "envd_cpu_s": round(cpu, 3) if cpu is not None else None,
        "cpu_bounds": {
            "lower_s": tree_cpu_s if upper is not None else None,
            "upper_s": upper,
            "status": "conditional" if upper is not None else "unresolved",
            "reason": "Whole envd CPU can include other clients; ps counters are sampled and rounded. Socket visibility and CPU overlap are not independently proven.",
        },
    }


def tree_rss_kib(root: int) -> int:
    rows = probe(["ps", "-axo", "pid=,ppid=,rss="]).split()
    children: dict[int, list[int]] = {}
    rss: dict[int, int] = {}
    for i in range(0, len(rows), 3):
        pid, ppid, kib = (int(v) for v in rows[i : i + 3])
        children.setdefault(ppid, []).append(pid)
        rss[pid] = kib
    if root not in rss:
        raise ValueError(f"process {root} absent from ps")
    total, stack = 0, [root]
    while stack:
        pid = stack.pop()
        total += rss.get(pid, 0)
        stack.extend(children.get(pid, []))
    return total


def snapshot() -> dict:
    return {
        "ts": time.time(),
        "load_avg": load_avg(),
        "swap": swap_usage(),
        "vm": vm_counters(),
        "runners": runner_services(),
    }


def snapshot_or_error() -> dict:
    try:
        return snapshot()
    except Exception as error:
        return {"error": f"{type(error).__name__}: {error}"}


def refusals(snap: dict, max_load1: float) -> list[dict]:
    if "error" in snap:
        return [{"condition": "probe_failed", "value": snap["error"]}]
    found = [
        {
            "condition": "runner_busy"
            if runner["state"] == "busy"
            else "runner_unknown",
            "value": runner["label"],
            "pid": runner["pid"],
        }
        for runner in snap["runners"]["active"]
        if runner["state"] != "idle"
    ]
    if snap["runners"]["unattributed_worker_pids"]:
        found.append(
            {
                "condition": "probe_failed",
                "value": "unattributed Runner.Worker processes: "
                + str(snap["runners"]["unattributed_worker_pids"]),
            }
        )
    load1 = snap["load_avg"][0]
    if load1 > max_load1:
        found.append({"condition": "load_avg_1m", "value": load1, "limit": max_load1})
    return found


def guard(max_load1: float) -> dict:
    start = snapshot_or_error()
    found = refusals(start, max_load1)
    baseline = {"requested_s": BASELINE_SECONDS, "start": start}
    if "error" not in start:
        t0 = time.monotonic()
        time.sleep(BASELINE_SECONDS)
        end = snapshot_or_error()
        baseline.update(elapsed_s=round(time.monotonic() - t0, 3), end=end)
        found.extend(refusals(end, max_load1))
        if "error" not in end:
            for key in ("pageouts", "swapouts"):
                delta = end["vm"][key] - start["vm"][key]
                baseline[key + "_delta"] = delta
                if delta < 0:
                    found.append(
                        {
                            "condition": "probe_failed",
                            "value": key + " counter decreased",
                        }
                    )
                elif delta > MAX_PAGING_DELTA:
                    found.append(
                        {
                            "condition": key + "_delta",
                            "value": delta,
                            "limit": MAX_PAGING_DELTA,
                        }
                    )
    else:
        end = start
    return {
        "type": "guard",
        "verdict": "refuse" if found else "pass",
        "thresholds": {"max_load1": max_load1, "max_paging_delta": MAX_PAGING_DELTA},
        "refusals": found,
        "baseline": baseline,
        "snapshot": end,
    }


def describe(refusal: dict) -> str:
    condition, value = refusal["condition"], refusal["value"]
    if condition in {"runner_busy", "runner_unknown"}:
        return f"{condition}: {value} (pid {refusal['pid']})"
    if condition == "probe_failed":
        return f"probe_failed: {value}"
    return f"{condition}: {value} > limit {refusal['limit']}"


def open_record(path: str):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    return open(path, "x", encoding="utf-8")


def envd_snapshot(project: str) -> dict:
    try:
        return {
            "status": "captured",
            "processes": attribute_processes(process_table(), None, project)["envd"],
        }
    except Exception as error:
        return {"status": "not_captured", "error": f"{type(error).__name__}: {error}"}


def owner_proof(proofs: list) -> dict:
    identified = [proof for proof in proofs if proof.get("status") == "identified"]
    identities = {(proof["pid"], proof["started"]) for proof in identified}
    if len(identities) != 1:
        return {
            "status": "unresolved",
            "reason": "no unique serving envd socket peer was observed",
        }
    result = dict(identified[0])
    result["sharing"] = (
        "shared"
        if any(p["sharing"] == "shared" for p in identified)
        else "no_other_peer_observed"
    )
    result["other_peer_pids"] = sorted(
        {pid for p in identified for pid in p["other_peer_pids"]}
    )
    result["socket_pairs"] = [pair for p in identified for pair in p["socket_pairs"]]
    result["identified_samples"] = len(identified)
    result["unresolved_samples"] = len(proofs) - len(identified)
    return result


def sample_loop(
    root: int,
    interval: float,
    t0: float,
    emit,
    stop: threading.Event,
    peak: list[int | None],
    *,
    project_root=None,
    envd_seen=None,
    proofs=None,
) -> None:
    next_tick = time.monotonic()
    while not stop.is_set():
        record = {
            "type": "sample",
            "t": round(time.monotonic() - t0, 3),
            "conditions": {},
            "content": {},
        }
        try:
            record["conditions"].update(
                load_avg=load_avg(),
                swap=swap_usage(),
                vm=vm_counters(),
                runners=runner_services(),
            )
        except Exception as error:
            record["conditions"]["error"] = f"{type(error).__name__}: {error}"
        try:
            if project_root is None:
                record["content"]["tree_rss_kib"] = tree_rss_kib(root)
            else:
                rows = process_table()
                proof = serving_envd(rows, root, project_root)
                proofs.append(proof)
                measured = attribute_processes(rows, root, project_root, owner=proof)
                candidates = measured.pop("envd")
                record["conditions"]["envd_candidates"] = [
                    {
                        key: value
                        for key, value in process.items()
                        if key not in ("combined_rss_kib", "tree_without_envd_rss_kib")
                    }
                    for process in candidates
                ]
                record["content"].update(measured, envd_owner=proof)
                for process in candidates:
                    identity = (process["pid"], process["started"])
                    seen = envd_seen.get(identity)
                    combined = process["combined_rss_kib"]
                    lower = process["tree_without_envd_rss_kib"]
                    old_combined = seen.get("max_combined_rss_kib") if seen else None
                    old_lower = (
                        seen.get("max_tree_without_envd_rss_kib") if seen else None
                    )
                    envd_seen[identity] = {
                        **process,
                        "in_tree": process["in_tree"] or bool(seen and seen["in_tree"]),
                        "max_combined_rss_kib": max(
                            value
                            for value in (combined, old_combined)
                            if value is not None
                        )
                        if combined is not None or old_combined is not None
                        else None,
                        "max_tree_without_envd_rss_kib": max(
                            value for value in (lower, old_lower) if value is not None
                        )
                        if lower is not None or old_lower is not None
                        else None,
                    }
            rss = record["content"]["tree_rss_kib"]
            if rss is not None:
                peak[0] = rss if peak[0] is None else max(peak[0], rss)
        except Exception as error:
            record["content"]["error"] = f"{type(error).__name__}: {error}"
        emit(record)
        next_tick += interval
        stop.wait(max(0.0, next_tick - time.monotonic()))


def run_recipe(
    command: list[str],
    out_path: str,
    label: str,
    interval: float,
    guard_record: dict | None,
    *,
    output=None,
    stdout=None,
    stderr=None,
    env=None,
    cwd=None,
    project_root=None,
    echo=True,
) -> dict:
    start = guard_record["snapshot"] if guard_record else snapshot_or_error()
    envd_before = envd_snapshot(project_root) if project_root is not None else None
    with (
        open_record(out_path) if output is None else contextlib.nullcontext(output)
    ) as out:
        lock = threading.Lock()

        def emit(record: dict) -> None:
            with lock:
                out.write(json.dumps(record) + "\n")
                out.flush()

        emit(
            {
                "type": "start",
                "label": label,
                "command": command,
                "guarded": guard_record is not None,
                "interval_s": interval,
                "guard": guard_record,
                "snapshot": start,
                "envd_before": envd_before,
            }
        )
        t0 = time.monotonic()
        proc = subprocess.Popen(
            command,
            stdout=stdout,
            stderr=stderr,
            env=env,
            cwd=cwd,
            start_new_session=True,
        )
        stop = threading.Event()
        peak: list[int | None] = [None]
        envd_seen = {}
        proofs = []
        sampler = threading.Thread(
            target=sample_loop,
            args=(proc.pid, interval, t0, emit, stop, peak),
            kwargs={
                "project_root": project_root,
                "envd_seen": envd_seen,
                "proofs": proofs,
            },
            daemon=True,
        )
        sampler.start()
        interrupted = False
        try:
            _, status, usage = os.wait4(proc.pid, 0)
        except KeyboardInterrupt:
            interrupted = True
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            _, status, usage = os.wait4(proc.pid, 0)
        wall = time.monotonic() - t0
        stop.set()
        sampler.join()
        exit_code = os.waitstatus_to_exitcode(status)
        proc.returncode = exit_code
        proof = owner_proof(proofs) if project_root is not None else None
        owner = (
            envd_seen.get((proof["pid"], proof["started"]))
            if proof and proof["status"] == "identified"
            else None
        )
        upper = owner.get("max_combined_rss_kib") if owner else None
        if upper is not None and peak[0] is not None:
            # A spawned envd is absent from the samples taken before it starts.
            upper = max(upper, peak[0])
        lower = owner.get("max_tree_without_envd_rss_kib") if owner else None
        end = {
            "type": "end",
            "label": label,
            "exit": exit_code,
            "wall_s": round(wall, 2),
            "cpu_s": round(usage.ru_utime + usage.ru_stime, 2),
            "tree_peak_rss_mib": None if peak[0] is None else round(peak[0] / 1024, 1),
            "tree_plus_envd_peak_rss_mib": None
            if upper is None
            else round(upper / 1024, 1),
            "tree_without_envd_peak_rss_mib": None
            if lower is None
            else round(lower / 1024, 1),
            "envd_ownership": proof,
            "envd_before": envd_before,
            "envd_after": envd_snapshot(project_root)
            if project_root is not None
            else None,
            "envd_observed": list(envd_seen.values()),
            "tree_peak_status": "not_captured" if peak[0] is None else "captured",
            "interrupted": interrupted,
            "max_single_rss_mib": round(usage.ru_maxrss / (1024 * 1024), 1),
            "snapshot": snapshot_or_error(),
        }
        emit(end)
    if interrupted:
        raise KeyboardInterrupt("measurement interrupted")
    if echo:
        print(json.dumps(end), flush=True)
    return end


def add_thresholds(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--max-load1",
        type=float,
        default=DEFAULT_MAX_LOAD1,
        help="refuse when 1-minute load average exceeds this (default: %(default)s; includes I/O-blocked threads)",
    )


def add_recipe_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--out", required=True, help="new JSONL file; an existing file is an error"
    )
    parser.add_argument("--label", default="")
    parser.add_argument(
        "--interval", type=float, default=1.0, help="seconds between samples"
    )
    parser.add_argument(
        "command", nargs=argparse.REMAINDER, help="recipe command, after --"
    )


def _main() -> int:
    parser = argparse.ArgumentParser(description=DESCRIPTION)
    modes = parser.add_subparsers(dest="mode", required=True)
    guard_parser = modes.add_parser(
        "guard", help="check the host and report observed values"
    )
    add_thresholds(guard_parser)
    guard_parser.add_argument(
        "--out", help="also write the guard record to this new JSONL file"
    )
    run_parser = modes.add_parser(
        "run", help="guard, then run the recipe only on a pass"
    )
    add_thresholds(run_parser)
    add_recipe_args(run_parser)
    add_recipe_args(
        modes.add_parser(
            "sample",
            help="run the recipe with sampling, no guard (not for measurement)",
        )
    )
    args = parser.parse_args()

    if args.mode == "guard":
        record = guard(args.max_load1)
        if args.out:
            with open_record(args.out) as out:
                out.write(json.dumps(record) + "\n")
        print(json.dumps(record), flush=True)
        for refusal in record["refusals"]:
            print(f"REFUSED {describe(refusal)}", file=sys.stderr)
        return 2 if record["refusals"] else 0

    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("no command")
    if args.interval <= 0:
        parser.error("--interval must be positive")

    if args.mode == "sample":
        return run_recipe(command, args.out, args.label, args.interval, None)["exit"]

    record = guard(args.max_load1)
    if record["refusals"]:
        with open_record(args.out) as out:
            out.write(json.dumps(record) + "\n")
        for refusal in record["refusals"]:
            print(f"REFUSED {describe(refusal)}", file=sys.stderr)
        return 2
    return run_recipe(command, args.out, args.label, args.interval, record)["exit"]


def main() -> int:
    try:
        return _main()
    except FileExistsError as error:
        print(f"error: output already exists: {error.filename}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
