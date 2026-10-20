#!/usr/bin/env python3
"""ADR 0040 measurement plan: wall, user+sys CPU and summed process-tree RSS of one command.

    scripts/measure-recipe.py [--label NAME] [--interval SECONDS] -- <command...>

Prints one JSON line. CPU comes from `os.wait4` rusage of the whole reaped tree, so it counts every
descendant. Peak memory is the maximum, over once-a-second samples, of the summed RSS of the
command's process tree; `ru_maxrss` alone is the largest single descendant and is reported
separately. Cache state is not manipulated.
"""
import argparse
import json
import os
import subprocess
import sys
import threading
import time


def tree_rss_kib(root: int) -> int:
	rows = subprocess.run(
		["ps", "-axo", "pid=,ppid=,rss="], capture_output=True, text=True, check=False
	).stdout.split()
	triples = [tuple(map(int, rows[i : i + 3])) for i in range(0, len(rows), 3)]
	children: dict[int, list[int]] = {}
	rss = {}
	for pid, ppid, kib in triples:
		children.setdefault(ppid, []).append(pid)
		rss[pid] = kib
	total, stack = 0, [root]
	while stack:
		pid = stack.pop()
		total += rss.get(pid, 0)
		stack.extend(children.get(pid, []))
	return total


def main() -> int:
	parser = argparse.ArgumentParser()
	parser.add_argument("--label", default="")
	parser.add_argument("--interval", type=float, default=1.0)
	parser.add_argument("command", nargs=argparse.REMAINDER)
	args = parser.parse_args()
	command = args.command[1:] if args.command[:1] == ["--"] else args.command
	if not command:
		parser.error("no command")

	start = time.monotonic()
	proc = subprocess.Popen(command)
	peak = 0
	stop = threading.Event()

	def sample() -> None:
		nonlocal peak
		while not stop.is_set():
			peak = max(peak, tree_rss_kib(proc.pid))
			stop.wait(args.interval)

	sampler = threading.Thread(target=sample, daemon=True)
	sampler.start()
	_, status, usage = os.wait4(proc.pid, 0)
	wall = time.monotonic() - start
	stop.set()
	sampler.join()

	print(
		json.dumps(
			{
				"label": args.label,
				"command": command,
				"exit": os.waitstatus_to_exitcode(status),
				"wall_s": round(wall, 2),
				"cpu_s": round(usage.ru_utime + usage.ru_stime, 2),
				"tree_peak_rss_mib": round(peak / 1024, 1),
				# macOS reports ru_maxrss in bytes.
				"max_single_rss_mib": round(usage.ru_maxrss / (1024 * 1024), 1),
			}
		),
		flush=True,
	)
	return 0


if __name__ == "__main__":
	sys.exit(main())
