#!/usr/bin/env bash
# grammar-study.sh <dev|release|probe> [reps]: research harness for docs/research/grammar-bundling.md
# (branch research/grammar-bundling-harness, never merged). Runs on a BuildBuddy runner through
# `just remote grammar-study <phase> [reps]` and prints one tab-separated `GS` line per measurement.
set -euo pipefail
phase=${1:?usage: grammar-study.sh dev|release|probe [reps]}
reps=${2:-5}
root=$HOME/.cache/grammar-study
rm -rf "$root"
mkdir -p "$root"
jobs=${GS_JOBS:-${CARGO_BUILD_JOBS:-4}}
common=(--locked -j "$jobs" --config 'resolver.feature-unification="selected"')
declare -A feats=(
	[all]="--no-default-features --features grammars-all"
	[hot]="--no-default-features --features grammars-hot"
	[none]="--no-default-features"
	[hot-wasm]="--no-default-features --features grammars-hot,wasm"
)
variants=(all none hot hot-wasm)
TIMEFORMAT='%R %U %S'

emit() {
	local IFS=$'\t'
	printf 'GS\t%s\n' "$*"
}

# Prints "wall user sys" for the command; its output goes to $root/last.log.
timed() {
	local t
	if ! t=$({ time "$@" >"$root/last.log" 2>&1; } 2>&1); then
		tail -60 "$root/last.log" >&2
		return 1
	fi
	echo "$t"
}

timings() {
	python3 - "$1" "$2" "$3" "$4" <<'PY'
import json, re, sys
html, phase, variant, rep = sys.argv[1:5]
units = json.loads(re.search(r"const UNIT_DATA = (\[.*?\]);\n", open(html).read(), re.S).group(1))
def grammar(u):
    return u["name"].startswith("tree-sitter-") and u["name"] != "tree-sitter-language"
end = max(u["start"] + u["duration"] for u in units)
total = sum(u["duration"] for u in units)
g = [u for u in units if grammar(u)]
g_run = sum(u["duration"] for u in g if "(run)" in u["target"])
g_all = sum(u["duration"] for u in g)
g_end = max((u["start"] + u["duration"] for u in g), default=0.0)
ast = [u for u in units if u["name"] == "omp-ast"]
ast_start = ast[0]["start"] if ast else 0.0
ast_dur = ast[0]["duration"] if ast else 0.0
print("GS\ttimings\t%s\t%s\t%s\t%d\t%.1f\t%.1f\t%d\t%.1f\t%.1f\t%.1f\t%.1f\t%.1f" % (
    phase, variant, rep, len(units), end, total, len(g), g_all, g_run, g_end, ast_start, ast_dur))
if rep == "1":
    for u in sorted(units, key=lambda u: -u["duration"]):
        print("GS\tunit\t%s\t%s\t%s\t%s\t%s\t%.2f\t%.2f" % (
            phase, variant, u["name"], u["version"], u["target"].strip() or "lib", u["start"], u["duration"]))
PY
}

cfg_rep=0
relink() {
	local v=$1 dir=$2 i out
	for i in 1 2 3; do
		cfg_rep=$((cfg_rep + 1))
		out=$(CARGO_TARGET_DIR=$dir cargo rustc -p omp-kb --bin omp-kb "${common[@]}" ${feats[$v]} -- \
			-Z time-passes --cfg "gs_rep_$cfg_rep" 2>&1) || { echo "$out" | tail -40 >&2; return 1; }
		emit relink "$v" "$i" "$(echo "$out" | awk '/run_linker/ {sub(/;/, "", $2); print $2; exit}')" \
			"$(echo "$out" | awk '/link_binary/ {sub(/;/, "", $2); print $2; exit}')"
	done
}

sizes() {
	local label=$1 v=$2 bin=$3
	emit "$label" "$v" "$(stat -c %s "$bin")" "$(size -A "$bin" | awk '$1 == ".text" {t=$2} $1 == ".rodata" {r=$2} $1 == ".data.rel.ro" {d=$2} END {print t "\t" r "\t" d}')" \
		"$(nm "$bin" 2>/dev/null | grep ' T tree_sitter_[a-z_]*$' | grep -vc external_scanner || true)" \
		"$(nm "$bin" 2>/dev/null | grep -cE ' T tree_sitter_(ocaml_interface|ocaml_type|php|dtd|markdown_inline)$' || true)"
}

echo "host: $(nproc) cpus, $(free -g | awk '/Mem:/ {print $2}') GB; jobs=$jobs; $(cc --version | head -1); $(rustc -V)"
cargo fetch --locked >/dev/null 2>&1

dev() {
	local r k v dir t
	for ((r = 1; r <= reps; r++)); do
		for ((k = 0; k < ${#variants[@]}; k++)); do
			v=${variants[$(((k + r - 1) % ${#variants[@]}))]}
			dir=$root/t-$v
			rm -rf "$dir"
			emit load dev "$v" "$r" "$(cut -d' ' -f1-3 /proc/loadavg)"
			t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-ast "${common[@]}" ${feats[$v]} --timings)
			emit cold-ast dev "$v" "$r" $t "$(du -sm "$dir" | cut -f1)"
			timings "$dir/cargo-timings/cargo-timing.html" dev "$v" "$r"
			t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-ast "${common[@]}" ${feats[$v]})
			emit noop-ast dev "$v" "$r" $t
			CARGO_TARGET_DIR=$dir cargo clean -p omp-ast >/dev/null 2>&1
			t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-ast "${common[@]}" ${feats[$v]})
			emit rebuild-ast dev "$v" "$r" $t
			t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-kb --bin omp-kb "${common[@]}" ${feats[$v]})
			emit build-kb dev "$v" "$r" $t
			sizes size-kb-dev "$v" "$dir/debug/omp-kb"
			CARGO_TARGET_DIR=$dir cargo clean -p omp-kb >/dev/null 2>&1
			t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-kb --bin omp-kb "${common[@]}" ${feats[$v]})
			emit rebuild-kb dev "$v" "$r" $t
			relink "$v" "$dir"
			rm -rf "$dir"
		done
	done
}

release() {
	local r k v dir t
	for ((r = 1; r <= reps; r++)); do
		for ((k = 0; k < ${#variants[@]}; k++)); do
			v=${variants[$(((k + r - 1) % ${#variants[@]}))]}
			dir=$root/r-$v
			rm -rf "$dir"
			emit load release "$v" "$r" "$(cut -d' ' -f1-3 /proc/loadavg)"
			t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-ast --release "${common[@]}" ${feats[$v]} --timings)
			emit cold-ast release "$v" "$r" $t "$(du -sm "$dir" | cut -f1)"
			timings "$dir/cargo-timings/cargo-timing.html" release "$v" "$r"
			t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-kb --bin omp-kb --release "${common[@]}" ${feats[$v]})
			emit build-kb release "$v" "$r" $t
			sizes size-kb-release "$v" "$dir/release/omp-kb"
			rm -rf "$dir"
		done
	done
}

probe() {
	local dir=$root/probe wasm=$root/wasm samples=$PWD/.grammar-study/samples bin r i n lang order bytes iters t
	t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-ast --release --example grammar_probe "${common[@]}" --features grammars-all,wasm)
	emit build-probe release all,wasm 1 $t
	bin=$dir/release/examples/grammar_probe
	"$bin" download "$wasm" "$reps"
	mapfile -t langs < <("$bin" list)
	n=${#langs[@]}
	for ((r = 1; r <= reps; r++)); do
		for ((i = 0; i < n; i++)); do
			lang=${langs[$(((i + r * 7) % n))]}
			order="wasm-first"
			if (((i + r) % 2 == 0)); then order=native-first; fi
			"$bin" first "$wasm" "$samples" "$lang" "$r" "$order"
		done
	done
	for lang in "${langs[@]}"; do
		bytes=$(stat -c %s "$samples/$lang.txt")
		iters=$(((2097152 + bytes - 1) / bytes))
		if ((iters < 25)); then iters=25; fi
		if ((iters > 2000)); then iters=2000; fi
		"$bin" steady "$wasm" "$samples" "$lang" "$iters"
	done
	rm -rf "$dir"
}

"$phase"
