#!/usr/bin/env python3
"""Benchmark the dev-profile codegen backend: Cranelift members against all-LLVM.

Answers the decision left open by docs/audits/cranelift-panic-cleanup.md §7:
should the dev `omp` binary move to LLVM? The audit measured this on a 4-core
x86_64 host (§6.4); this script repeats the same scenarios on the machine it
runs on, so the numbers can come from the team's Apple silicon Macs.

Variants, selected only on the command line, never by editing a file:
  cranelift  the repository's configuration (members on Cranelift)
  llvm       --config 'profile.dev.codegen-backend="llvm"'

Scenarios, run in this order for each variant in every repetition:
  cold          fresh target dir; `cargo build -p omp-app --bin omp`
  edit-core     one-line comment appended to crates/core/src/lib.rs; rebuild omp
  edit-app      one-line comment appended to crates/app/src/main.rs; rebuild omp
  test-after-dev
                first test build after the dev build:
                `cargo nextest run -p omp-e2e --tests --no-run`
  loop-dev      another omp-core edit, then rebuild omp ...
  loop-test     ... then rebuild the omp-e2e tests: one turn of the
                "cargo run, then just test" loop, where Cranelift dev and LLVM
                test builds compile edited members twice

Every edited file is restored byte for byte afterwards. Variants alternate
their order between repetitions (C,L then L,C) to spread thermal and
background drift. Crates are fetched once, untimed, before the first build.
RUSTC_WRAPPER is cleared so no compiler cache takes part. macOS cannot drop
the page cache without root, so unlike the audit's Linux runs the cold builds
start with whatever the OS has cached; the interleaving spreads that too.

Output, in --out (default target/bench-dev-backend):
  meta.json      host, toolchain, commit, power state
  results.jsonl  one record per measurement
  summary.md     mean and standard deviation per scenario and variant

Usage:
  scripts/bench-dev-backend.py [--reps N] [--out DIR] [--scratch DIR] [--smoke]

--smoke swaps every build for a tiny one (omp-core) to check the harness
itself in minutes; its numbers mean nothing.
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
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LLVM_DEV = ["--config", 'profile.dev.codegen-backend="llvm"']
VARIANTS = {"cranelift": [], "llvm": LLVM_DEV}
EDIT_CORE = ROOT / "crates/core/src/lib.rs"
EDIT_APP = ROOT / "crates/app/src/main.rs"
COMPILING = re.compile(r"^\s*Compiling ", re.MULTILINE)


@dataclass(frozen=True)
class Commands:
	dev: list[str]
	test: list[str]


FULL = Commands(
	dev=["cargo", "build", "-p", "omp-app", "--bin", "omp", "--locked"],
	test=["cargo", "nextest", "run", "-p", "omp-e2e", "--tests", "--no-run", "--locked"],
)
SMOKE = Commands(
	dev=["cargo", "build", "-p", "omp-core", "--locked"],
	test=["cargo", "nextest", "run", "-p", "omp-core", "--lib", "--no-run", "--locked"],
)


def capture(*command: str) -> str:
	try:
		return subprocess.run(
			command, cwd=ROOT, capture_output=True, text=True, check=False
		).stdout.strip()
	except FileNotFoundError:
		return ""


def host_meta() -> dict:
	meta = {
		"commit": capture("git", "rev-parse", "HEAD"),
		"dirty": bool(capture("git", "status", "--porcelain", "--untracked-files=no")),
		"rustc": capture("rustc", "-Vv"),
		"cargo": capture("cargo", "-V"),
		"nextest": capture("cargo", "nextest", "--version").splitlines()[:1],
		"platform": platform.platform(),
		"machine": platform.machine(),
		"logical_cpus": os.cpu_count(),
		"user_cargo_config": any(
			(Path.home() / ".cargo" / name).exists() for name in ("config.toml", "config")
		),
	}
	if sys.platform == "darwin":
		for key in ("hw.model", "hw.memsize", "hw.perflevel0.physicalcpu", "hw.perflevel1.physicalcpu"):
			meta[key] = capture("sysctl", "-n", key)
		meta["macos"] = capture("sw_vers", "-productVersion")
		# A laptop on battery or under thermal pressure builds measurably slower.
		meta["power"] = capture("pmset", "-g", "batt").splitlines()[:2]
		meta["thermal"] = capture("pmset", "-g", "therm")
	return meta


def dir_bytes(path: Path) -> int:
	total = 0
	for base, _, files in os.walk(path):
		for name in files:
			try:
				total += os.lstat(os.path.join(base, name)).st_size
			except OSError:
				pass
	return total


class Edit:
	"""Appends a comment line to a source file and restores the original bytes."""

	def __init__(self, path: Path):
		self.path = path
		self.original = path.read_bytes()
		self.count = 0

	def apply(self) -> None:
		self.count += 1
		self.path.write_bytes(self.original + f"\n// bench-dev-backend edit {self.count}\n".encode())

	def restore(self) -> None:
		self.path.write_bytes(self.original)


def timed(command: list[str], variant: str, env: dict) -> tuple[float, int]:
	# Appended, so it reaches `cargo build` and `cargo nextest run` alike;
	# nextest forwards --config to the cargo build it drives.
	full = command + VARIANTS[variant]
	started = time.monotonic()
	result = subprocess.run(full, cwd=ROOT, env=env, capture_output=True, text=True, check=False)
	elapsed = time.monotonic() - started
	if result.returncode != 0:
		sys.stderr.write(result.stderr[-8000:])
		raise SystemExit(f"command failed ({result.returncode}): {' '.join(full)}")
	return elapsed, len(COMPILING.findall(result.stderr))


def run_variant(variant: str, rep: int, commands: Commands, target: Path, env: dict, record) -> None:
	shutil.rmtree(target, ignore_errors=True)
	core, app = Edit(EDIT_CORE), Edit(EDIT_APP)
	try:
		steps = [
			("cold", None, commands.dev),
			("edit-core", core, commands.dev),
			("edit-app", app, commands.dev),
			("test-after-dev", None, commands.test),
			("loop-dev", core, commands.dev),
			("loop-test", None, commands.test),
		]
		for scenario, edit, command in steps:
			if edit is not None:
				edit.apply()
			seconds, units = timed(command, variant, env)
			record(variant=variant, rep=rep, scenario=scenario, seconds=round(seconds, 2), units=units)
			print(f"  {variant:9} rep {rep} {scenario:15} {seconds:8.1f} s  {units:4} units", flush=True)
	finally:
		core.restore()
		app.restore()
	record(variant=variant, rep=rep, scenario="target-dir", bytes=dir_bytes(target))
	shutil.rmtree(target, ignore_errors=True)


def summarize(rows: list[dict], meta: dict, reps: int, smoke: bool) -> str:
	scenarios = ["cold", "edit-core", "edit-app", "test-after-dev", "loop-dev", "loop-test"]

	def stats(variant: str, scenario: str) -> tuple[float, float, int] | None:
		values = [r["seconds"] for r in rows if r["variant"] == variant and r["scenario"] == scenario]
		units = [r["units"] for r in rows if r["variant"] == variant and r["scenario"] == scenario]
		if not values:
			return None
		sd = statistics.stdev(values) if len(values) > 1 else 0.0
		return statistics.mean(values), sd, round(statistics.median(units))

	lines = [
		"# Dev codegen backend benchmark",
		"",
		f"- Host: {meta.get('hw.model') or meta['platform']}, {meta['logical_cpus']} logical CPUs",
		f"- Commit: `{meta['commit'][:10]}`{' (dirty)' if meta['dirty'] else ''}",
		f"- Repetitions: {reps}{' — SMOKE RUN, numbers are meaningless' if smoke else ''}",
	]
	if meta.get("power"):
		lines.append(f"- Power: {' / '.join(meta['power'])}")
	lines += [
		"",
		"| Scenario | Cranelift (mean ± sd) | LLVM (mean ± sd) | LLVM vs Cranelift | Units C / L |",
		"|---|---:|---:|---:|---:|",
	]
	for scenario in scenarios:
		c, l = stats("cranelift", scenario), stats("llvm", scenario)
		if c is None or l is None:
			continue
		delta = (l[0] - c[0]) / c[0] * 100 if c[0] else 0.0
		lines.append(
			f"| {scenario} | {c[0]:.1f} ± {c[1]:.1f} s | {l[0]:.1f} ± {l[1]:.1f} s | {delta:+.1f}% | {c[2]} / {l[2]} |"
		)
	for variant in VARIANTS:
		loop = [
			sum(r["seconds"] for r in rows if r["variant"] == variant and r["rep"] == rep and r["scenario"] in ("loop-dev", "loop-test"))
			for rep in range(1, reps + 1)
		]
		sizes = [r["bytes"] for r in rows if r["variant"] == variant and r["scenario"] == "target-dir"]
		lines.append("")
		lines.append(
			f"- {variant}: edit, build and test loop {statistics.mean(loop):.1f} s per turn; "
			f"target dir {statistics.mean(sizes) / 1e9:.1f} GB after all scenarios"
		)
	return "\n".join(lines) + "\n"


def main() -> None:
	parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
	parser.add_argument("--reps", type=int, default=3)
	parser.add_argument("--out", type=Path, default=ROOT / "target/bench-dev-backend")
	parser.add_argument("--scratch", type=Path, default=None, help="where the throwaway target dir lives")
	parser.add_argument("--smoke", action="store_true")
	args = parser.parse_args()
	if args.reps < 1:
		parser.error("--reps must be at least 1")

	commands = SMOKE if args.smoke else FULL
	args.out.mkdir(parents=True, exist_ok=True)
	scratch = args.scratch or args.out / "scratch"
	target = scratch / "target"

	env = {k: v for k, v in os.environ.items() if k not in ("RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER")}
	env.update(CARGO_TARGET_DIR=str(target), CARGO_TERM_COLOR="never", CARGO_TERM_PROGRESS_WHEN="never")

	for path in (EDIT_CORE, EDIT_APP):
		if capture("git", "status", "--porcelain", "--", str(path.relative_to(ROOT))):
			raise SystemExit(f"{path} has uncommitted changes; the benchmark edits and restores it")

	meta = host_meta()
	(args.out / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
	subprocess.run(["cargo", "fetch", "--locked"], cwd=ROOT, env=env, check=True)

	rows: list[dict] = []
	results = (args.out / "results.jsonl").open("w")

	def record(**row) -> None:
		rows.append(row)
		results.write(json.dumps(row) + "\n")
		results.flush()

	order = list(VARIANTS)
	for rep in range(1, args.reps + 1):
		for variant in order if rep % 2 else reversed(order):
			print(f"rep {rep}/{args.reps}, variant {variant}", flush=True)
			run_variant(variant, rep, commands, target, env, record)
	results.close()

	summary = summarize(rows, meta, args.reps, args.smoke)
	(args.out / "summary.md").write_text(summary)
	print(summary)


if __name__ == "__main__":
	main()
