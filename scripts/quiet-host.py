#!/usr/bin/env python3
import argparse
import json
import os
import re
import subprocess
import sys
import threading
import time

RUNNER_PREFIX = "actions.runner."
DEFAULT_MAX_SWAP_USED_MIB = 1024.0
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
	values = [float(v) for v in probe(["sysctl", "-n", "vm.loadavg"]).strip(" {}\n").split()]
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
	for line in lines[1:]:
		fields = line.split("\t")
		if len(fields) != 3:
			raise ValueError(f"launchctl list row unparsable: {line!r}")
		pid, _, label = fields
		if not label.startswith(RUNNER_PREFIX):
			continue
		if pid == "-":
			inactive.append(label)
		else:
			active.append({"label": label, "pid": int(pid)})
	return {"active": active, "inactive": inactive}


def tree_rss_kib(root: int) -> int:
	rows = probe(["ps", "-axo", "pid=,ppid=,rss="]).split()
	children: dict[int, list[int]] = {}
	rss: dict[int, int] = {}
	for i in range(0, len(rows), 3):
		pid, ppid, kib = (int(v) for v in rows[i : i + 3])
		children.setdefault(ppid, []).append(pid)
		rss[pid] = kib
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


def refusals(snap: dict, max_swap_used_mib: float, max_load1: float) -> list[dict]:
	if "error" in snap:
		return [{"condition": "probe_failed", "value": snap["error"]}]
	found = [
		{"condition": "runner_active", "value": runner["label"], "pid": runner["pid"]}
		for runner in snap["runners"]["active"]
	]
	swap_used = snap["swap"]["used_mib"]
	if swap_used > max_swap_used_mib:
		found.append({"condition": "swap_used_mib", "value": swap_used, "limit": max_swap_used_mib})
	load1 = snap["load_avg"][0]
	if load1 > max_load1:
		found.append({"condition": "load_avg_1m", "value": load1, "limit": max_load1})
	return found


def guard(max_swap_used_mib: float, max_load1: float) -> dict:
	snap = snapshot_or_error()
	found = refusals(snap, max_swap_used_mib, max_load1)
	return {
		"type": "guard",
		"verdict": "refuse" if found else "pass",
		"thresholds": {"max_swap_used_mib": max_swap_used_mib, "max_load1": max_load1},
		"refusals": found,
		"snapshot": snap,
	}


def describe(refusal: dict) -> str:
	condition, value = refusal["condition"], refusal["value"]
	if condition == "runner_active":
		return f"runner_active: {value} (pid {refusal['pid']})"
	if condition == "probe_failed":
		return f"probe_failed: {value}"
	return f"{condition}: {value} > limit {refusal['limit']}"


def open_record(path: str):
	os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
	return open(path, "x", encoding="utf-8")


def sample_loop(root: int, interval: float, t0: float, emit, stop: threading.Event, peak: list[int]) -> None:
	next_tick = time.monotonic()
	while not stop.is_set():
		record = {"type": "sample", "t": round(time.monotonic() - t0, 3)}
		try:
			record.update(
				load_avg=load_avg(),
				swap=swap_usage(),
				vm=vm_counters(),
				tree_rss_kib=tree_rss_kib(root),
				runners_active=[runner["label"] for runner in runner_services()["active"]],
			)
			peak[0] = max(peak[0], record["tree_rss_kib"])
		except Exception as error:
			record["error"] = f"{type(error).__name__}: {error}"
		emit(record)
		next_tick += interval
		stop.wait(max(0.0, next_tick - time.monotonic()))


def run_recipe(command: list[str], out_path: str, label: str, interval: float, guard_record: dict | None) -> int:
	start = guard_record["snapshot"] if guard_record else snapshot_or_error()
	with open_record(out_path) as out:
		lock = threading.Lock()

		def emit(record: dict) -> None:
			with lock:
				out.write(json.dumps(record) + "\n")
				out.flush()

		emit({
			"type": "start",
			"label": label,
			"command": command,
			"guarded": guard_record is not None,
			"interval_s": interval,
			"guard": guard_record,
			"snapshot": start,
		})
		t0 = time.monotonic()
		proc = subprocess.Popen(command)
		stop = threading.Event()
		peak = [0]
		sampler = threading.Thread(
			target=sample_loop, args=(proc.pid, interval, t0, emit, stop, peak), daemon=True
		)
		sampler.start()
		_, status, usage = os.wait4(proc.pid, 0)
		wall = time.monotonic() - t0
		stop.set()
		sampler.join()
		exit_code = os.waitstatus_to_exitcode(status)
		end = {
			"type": "end",
			"label": label,
			"exit": exit_code,
			"wall_s": round(wall, 2),
			"cpu_s": round(usage.ru_utime + usage.ru_stime, 2),
			"tree_peak_rss_mib": round(peak[0] / 1024, 1),
			"max_single_rss_mib": round(usage.ru_maxrss / (1024 * 1024), 1),
			"snapshot": snapshot_or_error(),
		}
		emit(end)
	print(json.dumps(end), flush=True)
	return exit_code


def add_thresholds(parser: argparse.ArgumentParser) -> None:
	parser.add_argument(
		"--max-swap-used-mib", type=float, default=DEFAULT_MAX_SWAP_USED_MIB,
		help="refuse when swap used exceeds this many MiB (default: %(default)s; 1 GiB, about 6%% of 16 GiB RAM)",
	)
	parser.add_argument(
		"--max-load1", type=float, default=DEFAULT_MAX_LOAD1,
		help="refuse when the 1-minute load average exceeds this (default: %(default)s; one runnable thread on 10 cores)",
	)


def add_recipe_args(parser: argparse.ArgumentParser) -> None:
	parser.add_argument("--out", required=True, help="new JSONL file; an existing file is an error")
	parser.add_argument("--label", default="")
	parser.add_argument("--interval", type=float, default=1.0, help="seconds between samples")
	parser.add_argument("command", nargs=argparse.REMAINDER, help="recipe command, after --")


def main() -> int:
	parser = argparse.ArgumentParser(description=DESCRIPTION)
	modes = parser.add_subparsers(dest="mode", required=True)
	guard_parser = modes.add_parser("guard", help="check the host and report observed values")
	add_thresholds(guard_parser)
	guard_parser.add_argument("--out", help="also write the guard record to this new JSONL file")
	run_parser = modes.add_parser("run", help="guard, then run the recipe only on a pass")
	add_thresholds(run_parser)
	add_recipe_args(run_parser)
	add_recipe_args(modes.add_parser("sample", help="run the recipe with sampling, no guard (not for measurement)"))
	args = parser.parse_args()

	if args.mode == "guard":
		record = guard(args.max_swap_used_mib, args.max_load1)
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
		return run_recipe(command, args.out, args.label, args.interval, None)

	record = guard(args.max_swap_used_mib, args.max_load1)
	if record["refusals"]:
		with open_record(args.out) as out:
			out.write(json.dumps(record) + "\n")
		for refusal in record["refusals"]:
			print(f"REFUSED {describe(refusal)}", file=sys.stderr)
		return 2
	return run_recipe(command, args.out, args.label, args.interval, record)


if __name__ == "__main__":
	sys.exit(main())
