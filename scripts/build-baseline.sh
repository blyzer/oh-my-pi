#!/usr/bin/env bash
# build-baseline.sh [scenario... | summary | ci]
# Measures the local build/test/lint loop of this workspace (docs/research/build-baseline.md):
# a clean `just check`, a warm no-op, incremental checks after a one-line edit in a leaf
# (omp-core), mid-graph (omp-chat) and top (omp-app) crate, a cold `omp` build with
# `--timings`, `just build` no-op and leaf edit, clippy cold and warm, and `just test`.
#
# Every scenario runs in a dedicated target directory (BASELINE_TARGET_DIR, default
# /private/tmp/omp-build-baseline-target), so no other checkout's artifacts are touched.
# Warm and incremental scenarios repeat BASELINE_REPS times (default 3).
#
# Concurrent load. Before each timed run the script waits (up to BASELINE_COOLDOWN_S, default
# 60 s) for the 1-minute load average to fall below the CPU count. During the run it samples, every
# 5 s, the load average, the number of foreign compiler processes (rustc, clippy-driver, rustdoc,
# ld64.lld, or a bazel/java build server that is not a descendant of this script) and the summed
# %CPU of all non-descendant processes. A run is contaminated when its start load average is above
# the CPU count (flag 1), or a foreign compiler was seen or foreign CPU exceeded
# BASELINE_FOREIGN_CPU (flag 2; default 300, i.e. three cores; an idle desktop on the reference Mac
# measured ~200). A contaminated run is repeated once, and both attempts are recorded. The build's
# own load is not a signal: a cold build alone holds the load average above ten. On macOS the load
# average also counts threads blocked on I/O and swap-in, so under memory pressure it can exceed
# the CPU count on an otherwise idle machine; flag 2 is the one that names real competing work.
#
# When BASELINE_LOCK names an existing file (default /tmp/claude-501/omp2-cargo.lock, the
# machine-wide cargo lock), each timed command runs under `lockf -k` on it; the wait for the
# lock is not part of the timing.
#
# Edits are comment lines appended to <crate>/src/lib.rs and reverted from a copy after each
# repetition; the file is restored on exit even if a build fails.
#
# Output (BASELINE_OUT, default $BASELINE_TARGET_DIR/baseline): results.tsv (one row per
# attempt), logs/<scenario>-<rep>-<attempt>.log, cargo-timings/ from the cold build, and
# summary.md (median, min, max per scenario; critical path and top crates of the cold build).
#
# Needs: just, cargo-nextest, python3 (summary only), `just setup-python` done once.
# Scenarios (default: all, in this order):
#   check-clean check-noop check-edit-core check-edit-chat check-edit-app
#   build-cold build-noop build-edit-app clippy-cold clippy-noop test-cold test-warm
# `summary` alone rewrites summary.md from an existing results.tsv.
# `ci` alone (needs an authenticated `gh`) writes $OUT/ci/summary.md: per-job and per-step
# durations (median, p90) of recent successful ci.yml runs and the cache state of each job.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO_ROOT"

TARGET=${BASELINE_TARGET_DIR:-/private/tmp/omp-build-baseline-target}
OUT=${BASELINE_OUT:-$TARGET/baseline}
REPS=${BASELINE_REPS:-3}
COOLDOWN_S=${BASELINE_COOLDOWN_S:-60}
FOREIGN_CPU=${BASELINE_FOREIGN_CPU:-300}
LOCK=${BASELINE_LOCK:-/tmp/claude-501/omp2-cargo.lock}
MIN_FREE_GB=${BASELINE_MIN_FREE_GB:-25}
NCPU=$(sysctl -n hw.ncpu)
export CARGO_TARGET_DIR="$TARGET"

ALL=(check-clean check-noop check-edit-core check-edit-chat check-edit-app
	build-cold build-noop build-edit-app clippy-cold clippy-noop test-cold test-warm)

loadavg1() { sysctl -n vm.loadavg | awk '{print $2}'; }
free_gb() { echo $(($(df -Pk "$(dirname "$TARGET")" | awk 'NR == 2 { print $4 }') / 1048576)); }
gt() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a > b) }'; }

# Prints "<foreign compilers>\t<foreign %CPU>" for processes outside this script's process tree.
foreign_load() {
	ps -Aww -o pid=,ppid=,pcpu=,command= | awk -v root="$$" '
		{
			pid = $1; parent[pid] = $2; cpu[pid] = $3
			cmd = $0; sub(/^ *[0-9]+ +[0-9]+ +[0-9.]+ +/, "", cmd); split(cmd, argv, " ")
			compiler[pid] = argv[1] ~ /(^|\/)(rustc|clippy-driver|rustdoc|ld64\.lld)$/ || cmd ~ /^bazel\(/
		}
		END {
			for (pid in parent) {
				p = pid; ours = 0
				for (hops = 0; hops < 64 && p > 1; hops++) {
					if (p == root) { ours = 1; break }
					p = parent[p]
				}
				if (ours) continue
				c += cpu[pid]
				if (compiler[pid] && cpu[pid] > 1) n++
			}
			printf "%d\t%.0f\n", n, c
		}'
}

backup=""
restore_edit() {
	if [ -n "$backup" ]; then
		cp "$backup" "${backup%.baseline-orig}"
		rm -f "$backup"
		backup=""
	fi
}
trap restore_edit EXIT
trap 'restore_edit; exit 130' INT TERM

append_probe() { # <file> <tag>
	backup="$1.baseline-orig"
	cp "$1" "$backup"
	printf '// build-baseline probe %s %s\n' "$2" "$EPOCHREALTIME" >>"$1"
}

# Runs "$@" timed under the lock and the load sampler; appends one results.tsv row.
# Returns 0 when the run was clean, 2 when contaminated.
measure() { # <scenario> <rep> <attempt> <cmd...>
	local scenario=$1 rep=$2 attempt=$3
	shift 3
	local log="$OUT/logs/$scenario-$rep-$attempt.log" tfile samples waited=0
	tfile=$(mktemp)
	samples=$(mktemp)

	while gt "$(loadavg1)" "$NCPU" && [ "$waited" -lt "$COOLDOWN_S" ]; do
		sleep 10
		waited=$((waited + 10))
	done
	if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
		echo "error: less than ${MIN_FREE_GB} GB free; stopping" >&2
		exit 1
	fi

	local load_start
	load_start=$(loadavg1)
	(
		while :; do
			printf '%s\t%s\n' "$(loadavg1)" "$(foreign_load)" >>"$samples"
			sleep 5
		done
	) &
	local sampler=$!

	local lockcmd=()
	[ -f "$LOCK" ] && lockcmd=(lockf -k "$LOCK")
	local status=0
	"${lockcmd[@]}" /usr/bin/time -l -o "$tfile" "$@" >"$log" 2>&1 || status=$?
	kill "$sampler" 2>/dev/null || true
	wait "$sampler" 2>/dev/null || true

	local load_end wall user sys rss_mb load_max foreign_max fcpu_max units contaminated=0
	load_end=$(loadavg1)
	read -r wall user sys < <(awk 'NR == 1 { print $1, $3, $5 }' "$tfile")
	rss_mb=$(awk '/maximum resident set size/ { printf "%.0f", $1 / 1048576 }' "$tfile")
	load_max=$(awk -F'\t' 'BEGIN { m = 0 } $1 > m { m = $1 } END { print m }' "$samples")
	foreign_max=$(awk -F'\t' 'BEGIN { m = 0 } $2 > m { m = $2 } END { print m }' "$samples")
	fcpu_max=$(awk -F'\t' 'BEGIN { m = 0 } $3 > m { m = $3 } END { print m }' "$samples")
	units=$(grep -cE '^\s*(\x1b\[[0-9;]*m)*\s*(Compiling|Checking) ' "$log" || true)
	# 1: start load average above the CPU count; 2: foreign compiler or CPU (bit flags).
	if gt "$load_start" "$NCPU"; then contaminated=1; fi
	if [ "$foreign_max" -gt 0 ] || [ "$fcpu_max" -gt "$FOREIGN_CPU" ]; then contaminated=$((contaminated | 2)); fi
	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
		"$scenario" "$rep" "$attempt" "$status" "$wall" "$user" "$sys" "$rss_mb" \
		"$load_start" "$load_max" "$load_end" "$foreign_max" "$contaminated" "$units" \
		"$waited" "$(date -u +%FT%TZ)" "$fcpu_max" >>"$OUT/results.tsv"
	echo "$scenario rep=$rep attempt=$attempt status=$status wall=${wall}s units=$units load=$load_start/$load_max/$load_end foreign=$foreign_max foreign_cpu=$fcpu_max contaminated=$contaminated"
	rm -f "$tfile" "$samples"
	if [ "$status" -ne 0 ]; then
		echo "error: $scenario failed (status $status); see $log" >&2
		exit "$status"
	fi
	[ "$contaminated" -eq 0 ] || return 2
}

# A contaminated attempt is repeated once; both rows stay in results.tsv.
run() { # <scenario> <rep> <setup-fn|-> <cmd...>
	local scenario=$1 rep=$2 setup=$3
	shift 3
	local attempt
	for attempt in 1 2; do
		[ "$setup" = - ] || "$setup" "$scenario" "$rep" "$attempt"
		local rc=0
		measure "$scenario" "$rep" "$attempt" "$@" || rc=$?
		restore_edit
		[ "$rc" -eq 2 ] || return 0
	done
}

edit_core() { append_probe crates/core/src/lib.rs "$1-$2-$3"; }
edit_chat() { append_probe crates/chat/src/lib.rs "$1-$2-$3"; }
edit_app() { append_probe crates/app/src/lib.rs "$1-$2-$3"; }
clean_target() { rm -rf "$TARGET/debug" "$TARGET/clippy" "$TARGET/cargo-timings" "$TARGET/nextest" "$TARGET/.rustc_info.json" "$TARGET/CACHEDIR.TAG"; }
# Cold scenarios re-clean inside `run`, so a contaminated attempt 1 is retried cold, not warm.
clean_debug() { rm -rf "$TARGET/debug" "$TARGET/cargo-timings"; }
clean_clippy() { rm -rf "$TARGET/clippy"; }

# Edits leave artifacts built from the probe line; one untimed check resynchronises.
resync() { just check >/dev/null 2>&1; }

scenario() {
	local r
	case "$1" in
	check-clean) run check-clean 1 clean_target just check ;;
	check-noop) for r in $(seq "$REPS"); do run check-noop "$r" - just check; done ;;
	check-edit-core | check-edit-chat | check-edit-app)
		for r in $(seq "$REPS"); do run "$1" "$r" "edit_${1#check-edit-}" just check; done
		resync
		;;
	build-cold)
		run build-cold 1 clean_debug cargo build -p omp-app --bin omp --locked --timings
		mkdir -p "$OUT/cargo-timings"
		cp "$TARGET"/cargo-timings/cargo-timing.html "$OUT/cargo-timings/"
		;;
	build-noop) for r in $(seq "$REPS"); do run build-noop "$r" - just build; done ;;
	build-edit-app)
		for r in $(seq "$REPS"); do run build-edit-app "$r" edit_app just build; done
		just build >/dev/null 2>&1
		;;
	clippy-cold)
		run clippy-cold 1 clean_clippy env CARGO_TARGET_DIR="$TARGET/clippy" cargo clippy --workspace --all-targets --locked
		;;
	clippy-noop)
		for r in $(seq "$REPS"); do
			run clippy-noop "$r" - env CARGO_TARGET_DIR="$TARGET/clippy" cargo clippy --workspace --all-targets --locked
		done
		;;
	# A retry would find the test binaries built, so test-cold is measured once and only flagged.
	test-cold) measure test-cold 1 1 just test || true ;;
	test-warm) for r in $(seq "$REPS"); do run test-warm "$r" - just test; done ;;
	*)
		echo "unknown scenario: $1 (known: ${ALL[*]} summary)" >&2
		exit 64
		;;
	esac
	printf '%s\ttarget_gb=%s\tfree_gb=%s\n' "$1" "$(du -sk "$TARGET" | awk '{ printf "%.1f", $1 / 1048576 }')" "$(free_gb)" >>"$OUT/disk.tsv"
}

summary() {
	python3 - "$OUT" <<'PY'
import csv, json, re, statistics, sys
from pathlib import Path

out = Path(sys.argv[1])
cols = ["scenario", "rep", "attempt", "status", "wall", "user", "sys", "rss_mb", "load_start",
        "load_max", "load_end", "foreign_max", "contaminated", "units", "waited_s", "utc", "foreign_cpu"]
rows = [dict(zip(cols, r)) for r in csv.reader((out / "results.tsv").open(), delimiter="\t")]
lines = ["| Scenario | n (clean/total) | Median s | Min s | Max s | Spread s | Units (median) | Median user+sys s | Max RSS MB | Start load avg | Max foreign %CPU |",
         "|---|---|---|---|---|---|---|---|---|---|---|"]
order = []
for r in rows:
    if r["scenario"] not in order:
        order.append(r["scenario"])
for s in order:
    all_ = [r for r in rows if r["scenario"] == s]
    # Per repetition, the last attempt counts (a contaminated attempt 1 is superseded by attempt 2).
    last = {}
    for r in all_:
        last[r["rep"]] = r
    use = list(last.values())
    walls = [float(r["wall"]) for r in use]
    cpu = [float(r["user"]) + float(r["sys"]) for r in use]
    clean = sum(1 for r in use if r["contaminated"] == "0")
    loads = "/".join(f"{float(r['load_start']):.0f}" for r in use)
    fcpu = max(int(r.get("foreign_cpu") or 0) for r in use)
    lines.append(f"| {s} | {clean}/{len(use)} | {statistics.median(walls):.1f} | {min(walls):.1f} | "
                 f"{max(walls):.1f} | {max(walls) - min(walls):.1f} | {statistics.median(int(r['units']) for r in use):.0f} | "
                 f"{statistics.median(cpu):.1f} | {max(int(r['rss_mb'] or 0) for r in use)} | {loads} | {fcpu} |")

html = out / "cargo-timings" / "cargo-timing.html"
if html.exists():
    text = html.read_text()
    m = re.search(r"const UNIT_DATA = (\[.*?\]);\n", text, re.S)
    units = json.loads(m.group(1))
    by_index = {u["i"]: u for u in units}
    lines += ["", "Cold `cargo build -p omp-app --bin omp --timings`: top 10 units by duration", "",
              "| Unit | Version | Target | Duration s | Start s |", "|---|---|---|---|---|"]
    for u in sorted(units, key=lambda u: -u["duration"])[:10]:
        lines.append(f"| {u['name']} | {u['version']} | {u['target'].strip() or 'lib'} | {u['duration']:.1f} | {u['start']:.1f} |")
    # Critical path. Cargo records, per unit, the units it unblocked: those whose last outstanding
    # dependency it was, on finishing (`unblocked_units`) or on emitting metadata
    # (`unblocked_rmeta_units`, pipelined). Walk back from the last unit to finish through the unit
    # that unblocked each one; the walk is the chain of dependencies that gated the end time.
    unblocker = {}
    for u in units:
        for j in u.get("unblocked_units", []) + u.get("unblocked_rmeta_units", []):
            unblocker[j] = u["i"]
    cur = max(units, key=lambda u: u["start"] + u["duration"])
    path = [cur]
    while cur["i"] in unblocker:
        cur = by_index[unblocker[cur["i"]]]
        path.append(cur)
    path.reverse()
    total = path[-1]["start"] + path[-1]["duration"]
    on_path = sum(u["duration"] for u in path)
    lines += ["", f"Critical path ({len(path)} units, ends at {total:.1f} s; {on_path:.1f} s of unit time on it, "
              "pipelined units overlap their dependents):", "",
              "| # | Unit | Target | Start s | Duration s |", "|---|---|---|---|---|"]
    for n, u in enumerate(path, 1):
        lines.append(f"| {n} | {u['name']} | {u['target'].strip() or 'lib'} | {u['start']:.1f} | {u['duration']:.1f} |")
(out / "summary.md").write_text("\n".join(lines) + "\n")
print("\n".join(lines))
PY
}

# Collects CI durations and cache signals for ci.yml (gh, python3). Two samples: the last
# BASELINE_CI_RUNS successful omp2 pushes, and the last BASELINE_CI_RUNS successful pull-request
# runs whose macOS `rust` job ran and passed. BASELINE_CI_BEFORE (an ISO 8601 UTC time) keeps only
# runs created before it, so a report can be regenerated from the same runs. Job logs are cached
# under $OUT/ci/logs.
ci() {
	local repo=${BASELINE_CI_REPO:-blyzer/oh-my-pi}
	mkdir -p "$OUT/ci/logs"
	python3 - "$repo" "${BASELINE_CI_RUNS:-15}" "$OUT/ci" "${BASELINE_CI_BEFORE:-}" <<'PY'
import json, math, re, statistics, subprocess, sys
from datetime import datetime
from pathlib import Path

repo, want, out, before = sys.argv[1], int(sys.argv[2]), Path(sys.argv[3]), sys.argv[4]
RUST = "Rust workspace and acceptance proofs"
ANSI = re.compile(r"\x1b\[[0-9;]*m")

def gh(path, raw=False):
    args = ["gh", "api", *(["--allow-escape-sequences"] if raw else []), path]
    data = subprocess.check_output(args)
    return data if raw else json.loads(data)

def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00"))

def dur(o):
    return (ts(o["completed_at"]) - ts(o["started_at"])).total_seconds() if o.get("started_at") and o.get("completed_at") else None

def jobs_of(run):
    return gh(f"repos/{repo}/actions/runs/{run['id']}/jobs?per_page=100&filter=latest")["jobs"]

def collect(query, keep):
    picked, page = [], 1
    while len(picked) < want:
        runs = gh(f"repos/{repo}/actions/workflows/ci.yml/runs?{query}&status=success&per_page=50&page={page}")["workflow_runs"]
        if not runs:
            break
        for run in runs:
            if before and run["created_at"] >= before:
                continue
            jobs = jobs_of(run)
            if keep(jobs):
                picked.append((run, jobs))
            if len(picked) == want:
                break
        page += 1
    return picked

def rust_ran(jobs):
    return any(j["name"] == RUST and j["conclusion"] == "success" for j in jobs)

samples = {
    "push-omp2": collect("branch=omp2&event=push", lambda jobs: True),
    "pr-with-rust": collect("event=pull_request", rust_ran),
}

def pct(v, p):
    v = sorted(v)
    k = (len(v) - 1) * p
    lo, hi = math.floor(k), math.ceil(k)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)

def m(v):
    return f"{v / 60:.1f}"

def log_of(job):
    path = out / "logs" / f"{job['id']}.log"
    if not path.exists() or path.stat().st_size == 0:
        path.write_bytes(gh(f"repos/{repo}/actions/jobs/{job['id']}/logs", raw=True))
    return ANSI.sub("", path.read_text(errors="replace"))

FAMILY = {"License policy and release notices": "compliance", "Error-formatting ratchet (lintx)": "lintx"}

def cache_state(job, run):
    text = log_of(job)
    fam = FAMILY.get(job["name"], "cargo")
    restored = [k for k in re.findall(r"Cache restored from key: (\S+)", text) if k.startswith(fam)]
    missed = [k for k in re.findall(r"Cache not found for input keys: (\S+?)[,\s]", text) if k.startswith(fam)]
    if restored:
        state = "prefix" if fam == "cargo" and not restored[0].endswith(run["head_sha"]) else "exact"
    else:
        state = "miss" if missed else "n/a"
    units = len(re.findall(r"\s(Compiling|Checking) [\w-]+ v", text))
    reserve = len(re.findall(r"Unable to reserve cache", text))
    return state, units, reserve

lines = [f"Generated {datetime.now().astimezone():%Y-%m-%d %H:%M %Z} from {repo} ci.yml.", ""]
for label, picked in samples.items():
    runs = [r for r, _ in picked]
    lines += [f"## {label}: {len(picked)} runs, {min(r['created_at'] for r in runs)} .. {max(r['created_at'] for r in runs)}", "",
              "Runs: " + ", ".join(str(r["id"]) for r in runs), "",
              "| Job | n | Median min | p90 min | Max min |", "|---|---|---|---|---|"]
    per_job = {}
    for _, jobs in picked:
        for j in jobs:
            if j["conclusion"] == "success":
                per_job.setdefault(j["name"], []).append(dur(j))
    for name, v in sorted(per_job.items(), key=lambda kv: -statistics.median(kv[1])):
        lines.append(f"| {name} | {len(v)} | {m(statistics.median(v))} | {m(pct(v, .9))} | {m(max(v))} |")
    wall = [(ts(r["updated_at"]) - ts(r["run_started_at"])).total_seconds() for r in runs]
    lines += [f"| (run: started to last update) | {len(wall)} | {m(statistics.median(wall))} | {m(pct(wall, .9))} | {m(max(wall))} |", ""]

pooled = [x for picked in samples.values() for x in picked]
lines += ["## Steps of the heavy jobs (both samples pooled; steps with median < 5 s omitted)", ""]
for name in [RUST, "Lint workspace (Linux)", "Terminal proof P7 (Linux PTY)",
             "Acceptance proofs P9, P10 and tool sources (Linux)", "License policy and release notices"]:
    steps, order = {}, []
    for _, jobs in pooled:
        for j in jobs:
            if j["name"] != name or j["conclusion"] != "success":
                continue
            for s in j["steps"]:
                d = dur(s)
                if d is None or s["conclusion"] != "success":
                    continue
                if s["name"] not in order:
                    order.append(s["name"])
                steps.setdefault(s["name"], []).append(d)
    lines += [f"### {name}", "", "| Step | n | Median min | p90 min | Max min |", "|---|---|---|---|---|"]
    for s in order:
        v = steps[s]
        if statistics.median(v) >= 5 or max(v) >= 30:
            lines.append(f"| {s} | {len(v)} | {m(statistics.median(v))} | {m(pct(v, .9))} | {m(max(v))} |")
    lines.append("")

lines += ["## Cache restore state per job (both samples pooled)", "",
          "| Job | Event | exact | prefix | miss | Median units compiled (hit / miss) | Median job min (hit / miss) | 'Unable to reserve cache' |",
          "|---|---|---|---|---|---|---|---|"]
table = {}
for run, jobs in pooled:
    for j in jobs:
        if j["conclusion"] != "success" or j["name"] not in (RUST, *FAMILY, "Lint workspace (Linux)",
                "Terminal proof P7 (Linux PTY)", "Acceptance proofs P9, P10 and tool sources (Linux)"):
            continue
        state, units, reserve = cache_state(j, run)
        row = table.setdefault((j["name"], run["event"]), {"exact": 0, "prefix": 0, "miss": 0, "n/a": 0,
                                                           "hit_u": [], "miss_u": [], "hit_d": [], "miss_d": [], "reserve": 0})
        row[state] += 1
        row["reserve"] += reserve
        bucket = "miss" if state == "miss" else "hit"
        row[f"{bucket}_u"].append(units)
        row[f"{bucket}_d"].append(dur(j))
med = lambda v, f: f(statistics.median(v)) if v else "-"
for (name, event), row in sorted(table.items()):
    lines.append(f"| {name} | {event} | {row['exact']} | {row['prefix']} | {row['miss']} | "
                 f"{med(row['hit_u'], lambda x: f'{x:.0f}')} / {med(row['miss_u'], lambda x: f'{x:.0f}')} | "
                 f"{med(row['hit_d'], m)} / {med(row['miss_d'], m)} | {row['reserve']} |")
(out / "summary.md").write_text("\n".join(lines) + "\n")
print("\n".join(lines))
PY
}

mkdir -p "$OUT/logs"
case "${1-}" in
summary)
	summary
	exit 0
	;;
ci)
	ci
	exit 0
	;;
esac
[ -f vendor/python/pyo3-config.txt ] || just setup-python
cargo fetch --locked >/dev/null
{
	echo "host: $(sysctl -n hw.model) $(sysctl -n machdep.cpu.brand_string), ncpu=$NCPU, mem=$(($(sysctl -n hw.memsize) / 1073741824)) GB"
	echo "commit: $(git rev-parse HEAD)"
	echo "toolchain: $(rustc -V); $(cargo -V); $(cargo nextest --version | head -1)"
	echo "target: $TARGET; reps: $REPS; lock: $([ -f "$LOCK" ] && echo "$LOCK" || echo none)"
} >>"$OUT/meta.txt"
if [ "$#" -eq 0 ]; then set -- "${ALL[@]}"; fi
for s in "$@"; do scenario "$s"; done
summary
