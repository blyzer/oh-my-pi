#!/usr/bin/env python3
"""Measure sccache for the cases a persistent target dir does not cover.

Experimental: answers whether a shared local sccache pays off when one machine
builds several PRs, worktrees or checkouts of this repository. Nothing here
changes the repository's configuration; the build uses omp2's dev profile as
it stands (Cranelift members), so the only variable is the compiler cache.

Arms, each started from an empty cache:
  none              no compiler wrapper (the control)
  sccache           RUSTC_WRAPPER=sccache, default hashing (absolute paths
                    must match for a hit)
  sccache-basedirs  as above, with SCCACHE_BASEDIRS set to both worktree roots,
                    so paths inside either tree hash the same

Steps, in order, for every arm; the build is `cargo build -p omp-app --bin omp`:
  fresh-target-empty-cache  worktree A, empty target dir, empty cache
  fresh-target-warm-cache   worktree A, target dir deleted, cache from the
                            previous step (for `none`: a second cold build)
  second-worktree           worktree B at the same commit, its own empty
                            target dir, shared cache
  switch-to-change          worktree A, warm target dir, one-line comment
                            appended to crates/core/src/lib.rs (41 units)
  switch-back               worktree A, the edit reverted: the target dir now
                            holds the edited build, the cache the original

Recorded per step: wall time, units cargo compiled, sccache hits, misses and
non-cacheable calls for that step alone, cache size and target size.

Target dirs are named `*.noindex` (under each tree's target/), so Spotlight skips them for the length of
the run; other scanners (endpoint security) may still read the files. Arms
alternate order between repetitions. The sccache server runs on its own port
with its own cache directory under --scratch and is stopped at the end, so an
sccache the user already runs is left alone.

Usage:
  scripts/bench-sccache.py --sccache PATH [--reps N] [--out DIR] [--scratch DIR] [--smoke]
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ["cargo", "build", "-p", "omp-app", "--bin", "omp", "--locked"]
SMOKE_BUILD = ["cargo", "build", "-p", "omp-core", "--locked"]
EDIT = Path("crates/core/src/lib.rs")
COMPILING = re.compile(r"^\s*Compiling ", re.MULTILINE)
ARMS = ["none", "sccache", "sccache-basedirs"]
STEPS = [
	"fresh-target-empty-cache",
	"fresh-target-warm-cache",
	"second-worktree",
	"switch-to-change",
	"switch-back",
]
PORT = "4299"


def capture(*command: str, env: dict | None = None) -> str:
	try:
		return subprocess.run(
			command, cwd=ROOT, env=env, capture_output=True, text=True, check=False
		).stdout.strip()
	except FileNotFoundError:
		return ""


def dir_bytes(path: Path) -> int:
	total = 0
	for base, _, files in os.walk(path):
		for name in files:
			try:
				total += os.lstat(os.path.join(base, name)).st_size
			except OSError:
				pass
	return total


def host_meta(sccache: str) -> dict:
	meta = {
		"commit": capture("git", "rev-parse", "HEAD"),
		"dirty": bool(capture("git", "status", "--porcelain", "--untracked-files=no")),
		"rustc": capture("rustc", "-Vv"),
		"sccache": capture(sccache, "--version"),
		"platform": platform.platform(),
		"logical_cpus": os.cpu_count(),
	}
	if sys.platform == "darwin":
		for key in ("hw.model", "hw.memsize", "hw.perflevel0.physicalcpu", "hw.perflevel1.physicalcpu"):
			meta[key] = capture("sysctl", "-n", key)
		meta["macos"] = capture("sw_vers", "-productVersion")
		meta["power"] = capture("pmset", "-g", "batt").splitlines()[:2]
		meta["thermal"] = capture("pmset", "-g", "therm")
	return meta


class Cache:
	"""One sccache server with its own port and directory, for one arm."""

	def __init__(self, binary: str, directory: Path, basedirs: list[Path] | None):
		self.binary = binary
		self.directory = directory
		self.env = {
			**os.environ,
			"SCCACHE_SERVER_PORT": PORT,
			"SCCACHE_DIR": str(directory),
			"SCCACHE_CACHE_SIZE": "50G",
			"SCCACHE_IDLE_TIMEOUT": "0",
		}
		self.env.pop("SCCACHE_BASEDIRS", None)
		if basedirs:
			self.env["SCCACHE_BASEDIRS"] = ":".join(str(p) for p in basedirs)

	def start(self) -> None:
		shutil.rmtree(self.directory, ignore_errors=True)
		self.directory.mkdir(parents=True)
		subprocess.run([self.binary, "--stop-server"], env=self.env, capture_output=True, check=False)
		subprocess.run([self.binary, "--start-server"], env=self.env, check=True, capture_output=True)

	def stop(self) -> None:
		subprocess.run([self.binary, "--stop-server"], env=self.env, capture_output=True, check=False)

	def zero(self) -> None:
		subprocess.run([self.binary, "--zero-stats"], env=self.env, capture_output=True, check=True)

	def stats(self) -> dict:
		raw = capture(self.binary, "--show-stats", "--stats-format", "json", env=self.env)
		stats = json.loads(raw)["stats"]
		return {
			"hits": sum(stats["cache_hits"]["counts"].values()),
			"misses": sum(stats["cache_misses"]["counts"].values()),
			"non_cacheable": stats["requests_not_cacheable"],
		}


def build(tree: Path, target: Path, cache: Cache | None, command: list[str]) -> tuple[float, int]:
	env = {**os.environ, "CARGO_TARGET_DIR": str(target), "CARGO_TERM_COLOR": "never"}
	env.pop("RUSTC_WRAPPER", None)
	env.pop("CARGO_BUILD_RUSTC_WRAPPER", None)
	if cache is not None:
		env.update({k: v for k, v in cache.env.items() if k.startswith("SCCACHE_")})
		env["RUSTC_WRAPPER"] = cache.binary
	started = time.monotonic()
	result = subprocess.run(command, cwd=tree, env=env, capture_output=True, text=True, check=False)
	elapsed = time.monotonic() - started
	if result.returncode != 0:
		sys.stderr.write(result.stderr[-8000:])
		raise SystemExit(f"build failed in {tree}")
	return elapsed, len(COMPILING.findall(result.stderr))


def run_arm(
	arm: str, rep: int, trees: tuple[Path, Path], scratch: Path, sccache: str, command: list[str], record
) -> None:
	tree_a, tree_b = trees
	# Inside each tree, so SCCACHE_BASEDIRS covers the target paths the
	# compiler is handed as well as the sources.
	target_a, target_b = tree_a / "target/bench.noindex", tree_b / "target/bench.noindex"
	cache = None
	if arm != "none":
		cache = Cache(sccache, scratch / f"sccache-{arm}", [tree_a, tree_b] if arm == "sccache-basedirs" else None)
		cache.start()
	edit = tree_a / EDIT
	original = edit.read_bytes()
	try:
		for step in STEPS:
			tree, target = (tree_b, target_b) if step == "second-worktree" else (tree_a, target_a)
			if step in ("fresh-target-empty-cache", "fresh-target-warm-cache", "second-worktree"):
				shutil.rmtree(target, ignore_errors=True)
			if step == "switch-to-change":
				edit.write_bytes(original + f"\n// bench-sccache change rep {rep}\n".encode())
			if step == "switch-back":
				edit.write_bytes(original)
			if cache:
				cache.zero()
			seconds, units = build(tree, target, cache, command)
			row = {"arm": arm, "rep": rep, "step": step, "seconds": round(seconds, 2), "units": units}
			if cache:
				row.update(cache.stats(), cache_bytes=dir_bytes(cache.directory))
			row["target_bytes"] = dir_bytes(target)
			record(**row)
			hits = f"hits {row.get('hits', '-'):>5} misses {row.get('misses', '-'):>5}"
			print(f"  {arm:16} rep {rep} {step:24} {seconds:8.1f} s {units:4} units  {hits}", flush=True)
	finally:
		edit.write_bytes(original)
		if cache:
			cache.stop()
			shutil.rmtree(cache.directory, ignore_errors=True)
		for target in (target_a, target_b):
			shutil.rmtree(target, ignore_errors=True)


def summarize(rows: list[dict], meta: dict, reps: int) -> str:
	def mean(arm: str, step: str, key: str) -> float | None:
		values = [r[key] for r in rows if r["arm"] == arm and r["step"] == step and key in r]
		return statistics.mean(values) if values else None

	def sd(arm: str, step: str) -> float:
		values = [r["seconds"] for r in rows if r["arm"] == arm and r["step"] == step]
		return statistics.stdev(values) if len(values) > 1 else 0.0

	lines = [
		"# sccache benchmark",
		"",
		f"- Host: {meta.get('hw.model') or meta['platform']}, {meta['logical_cpus']} logical CPUs",
		f"- Commit: `{meta['commit'][:10]}`{' (dirty)' if meta['dirty'] else ''}; {meta['sccache']}",
		f"- Repetitions: {reps}",
	]
	if meta.get("power"):
		lines.append(f"- Power: {' / '.join(meta['power'])}")
	lines += [
		"",
		"Wall time (mean ± sd), and sccache hits / misses / non-cacheable calls for the step.",
		"",
		"| Step | none | sccache | sccache-basedirs |",
		"|---|---:|---:|---:|",
	]
	for step in STEPS:
		cells = []
		for arm in ARMS:
			seconds = mean(arm, step, "seconds")
			if seconds is None:
				cells.append("—")
				continue
			cell = f"{seconds:.1f} ± {sd(arm, step):.1f} s"
			if arm != "none":
				cell += f" ({mean(arm, step, 'hits'):.0f} / {mean(arm, step, 'misses'):.0f} / {mean(arm, step, 'non_cacheable'):.0f})"
			cells.append(cell)
		lines.append(f"| {step} | " + " | ".join(cells) + " |")
	lines.append("")
	for arm in ARMS[1:]:
		size = [r["cache_bytes"] for r in rows if r["arm"] == arm and "cache_bytes" in r]
		if size:
			lines.append(f"- {arm}: cache {max(size) / 1e9:.2f} GB at its largest")
	target = [r["target_bytes"] for r in rows if r["step"] == "fresh-target-empty-cache"]
	if target:
		lines.append(f"- target dir after one cold build: {statistics.mean(target) / 1e9:.1f} GB")
	return "\n".join(lines) + "\n"


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
	parser.add_argument("--sccache", required=True, help="path to the sccache binary")
	parser.add_argument("--reps", type=int, default=2)
	parser.add_argument("--out", type=Path, default=ROOT / "target/bench-sccache")
	parser.add_argument("--scratch", type=Path, default=None)
	parser.add_argument("--smoke", action="store_true", help="build omp-core only, to check the harness")
	args = parser.parse_args()
	if args.reps < 1:
		parser.error("--reps must be at least 1")
	if capture("git", "status", "--porcelain", "--", str(EDIT)):
		raise SystemExit(f"{EDIT} has uncommitted changes; the benchmark edits and restores it")

	args.out.mkdir(parents=True, exist_ok=True)
	scratch = (args.scratch or args.out / "scratch").resolve()
	scratch.mkdir(parents=True, exist_ok=True)
	meta = host_meta(args.sccache)
	(args.out / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
	subprocess.run(["cargo", "fetch", "--locked"], cwd=ROOT, check=True)

	# Worktree B: same commit, different absolute path, sharing vendor/ (the
	# embedded CPython) through a symlink so both trees link the same files.
	tree_b = scratch / "worktree-b"
	subprocess.run(["git", "worktree", "remove", "--force", str(tree_b)], cwd=ROOT, capture_output=True)
	subprocess.run(["git", "worktree", "add", "--detach", str(tree_b), "HEAD"], cwd=ROOT, check=True)
	(tree_b / "vendor").symlink_to((ROOT / "vendor").resolve())

	rows: list[dict] = []
	results = (args.out / "results.jsonl").open("w")

	def record(**row) -> None:
		rows.append(row)
		results.write(json.dumps(row) + "\n")
		results.flush()

	try:
		for rep in range(1, args.reps + 1):
			for arm in ARMS if rep % 2 else list(reversed(ARMS)):
				print(f"rep {rep}/{args.reps}, arm {arm}", flush=True)
				run_arm(arm, rep, (ROOT, tree_b), scratch, args.sccache, SMOKE_BUILD if args.smoke else BUILD, record)
	finally:
		results.close()
		subprocess.run(["git", "worktree", "remove", "--force", str(tree_b)], cwd=ROOT, capture_output=True)

	summary = summarize(rows, meta, args.reps)
	if args.smoke:
		summary = summary.replace("# sccache benchmark", "# sccache benchmark — SMOKE RUN (omp-core only), numbers are meaningless")
	(args.out / "summary.md").write_text(summary)
	print(summary)


if __name__ == "__main__":
	main()
