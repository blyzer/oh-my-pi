#!/usr/bin/env bash
# grammar-study.sh <dev|release|probe> [reps]: research harness for docs/research/grammar-bundling.md
# (branch research/grammar-bundling-harness, never merged). Runs on a BuildBuddy runner through
# `just remote grammar-study <phase> [reps]` and prints one tab-separated `GS` line per measurement.
set -euo pipefail
phase=${1:?usage: grammar-study.sh check|dev|release|probe [reps] [jobs]}
reps=${2:-5}
root=$HOME/.cache/grammar-study
rm -rf "$root"
mkdir -p "$root"
jobs=${3:-${CARGO_BUILD_JOBS:-4}}
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

check() {
	local v
	for v in "${variants[@]}"; do
		CARGO_TARGET_DIR=$root/check cargo check -p omp-ast -p omp-kb "${common[@]}" ${feats[$v]} --message-format short
		emit check "$v" ok
	done
	CARGO_TARGET_DIR=$root/check cargo check -p omp-ast --example grammar_probe "${common[@]}" --features grammars-all,wasm --message-format short
	emit check probe ok
	rm -rf "$root/check"
}

compressed() {
	emit bytes "$1" "$(stat -c %s "$2")" "$(gzip -9 -c "$2" | wc -c)" "$(zstd -19 -T0 -q -c "$2" | wc -c)"
}

# Release binary sizes, raw and compressed: omp-kb per grammar set, and the wasmtime runtime as a
# loader links it (wasm_runtime_size) against the same program without wasm calls.
bytes() {
	local dir=$root/bytes v
	for v in all hot none; do
		CARGO_TARGET_DIR=$dir cargo build -p omp-kb --bin omp-kb --release "${common[@]}" ${feats[$v]} >"$root/last.log" 2>&1 || { tail -40 "$root/last.log"; return 1; }
		cp "$dir/release/omp-kb" "$root/omp-kb-$v"
		compressed "omp-kb-$v" "$root/omp-kb-$v"
	done
	CARGO_TARGET_DIR=$dir cargo build -p omp-ast --release --example wasm_runtime_size --example native_runtime_size \
		"${common[@]}" --no-default-features --features grammars-hot,wasm >"$root/last.log" 2>&1 || { tail -40 "$root/last.log"; return 1; }
	compressed wasm_runtime_size "$dir/release/examples/wasm_runtime_size"
	compressed native_runtime_size "$dir/release/examples/native_runtime_size"
	rm -rf "$dir"
}


# Cold omp-ast builds with the four multi-grammar crates as shipped ("stock") and patched to compile
# only the parser omp-ast references ("trimmed"). Each patch is a thin crate generated outside the
# workspace; its build script compiles from the registry sources. Cargo.lock is restored after every
# build, and the archive member counts prove which parsers each arm compiled.
registry_dir() {
	find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -mindepth 2 -maxdepth 2 -type d -name "$1" | head -1
}

# make_wrapper CRATE VERSION LIBNAME SOURCE_DIR INCLUDE_DIR SYMBOL CONST [FLAG]
make_wrapper() {
	local crate=$1 version=$2 libname=$3 src=$4 inc=$5 symbol=$6 const=$7 flag=${8:-} dir reg flagline=""
	reg=$(registry_dir "$crate-$version")
	if [ -z "$reg" ]; then
		echo "error: $crate-$version is not in the Cargo registry" >&2
		return 1
	fi
	if [ -n "$flag" ]; then flagline="c.flag_if_supported(\"$flag\");"; fi
	dir=$root/patched/$crate
	mkdir -p "$dir"
	cat >"$dir/Cargo.toml" <<EOF
[package]
name = "$crate"
version = "$version"
edition = "2021"
build = "build.rs"

[lib]
path = "lib.rs"

[dependencies]
tree-sitter-language = "0.1"

[build-dependencies]
cc = "1.2"
EOF
	cat >"$dir/build.rs" <<EOF
fn main() {
    let root = std::path::Path::new("$reg");
    let mut c = cc::Build::new();
    c.std("c11").include(root.join("$inc"));
    $flagline
    c.file(root.join("$src/parser.c")).file(root.join("$src/scanner.c"));
    c.compile("$libname");
}
EOF
	cat >"$dir/lib.rs" <<EOF
use tree_sitter_language::LanguageFn;

extern "C" {
    fn $symbol() -> *const ();
}

pub const $const: LanguageFn = unsafe { LanguageFn::from_raw($symbol) };
EOF
	patches+=(--config "patch.crates-io.$crate.path=\"$dir\"")
	archives+=("$crate:$libname")
}

subgrammar() {
	local spec profile j n r arm dir t lock=$root/Cargo.lock.stock entry crate libname lib phase
	local -a flags arms
	patches=()
	archives=()
	make_wrapper tree-sitter-ocaml 0.24.2 tree-sitter-ocaml grammars/ocaml/src grammars/ocaml/src tree_sitter_ocaml LANGUAGE_OCAML
	make_wrapper tree-sitter-php 0.24.2 tree-sitter-php php_only/src php/src tree_sitter_php_only LANGUAGE_PHP_ONLY
	make_wrapper tree-sitter-md 0.5.3 tree-sitter-markdown tree-sitter-markdown/src tree-sitter-markdown/src tree_sitter_markdown LANGUAGE
	make_wrapper tree-sitter-xml 0.7.0 tree-sitter-xml xml/src xml/src tree_sitter_xml LANGUAGE_XML -Wno-unused-value
	cp Cargo.lock "$lock"
	for spec in dev:4:5 dev:11:5 release:4:3; do
		IFS=: read -r profile j n <<<"$spec"
		phase=sub-$profile-j$j
		flags=(-j "$j" --offline --config 'resolver.feature-unification="selected"' --no-default-features --features grammars-all --timings)
		if [ "$profile" = release ]; then flags+=(--release); fi
		for ((r = 1; r <= n; r++)); do
			arms=(stock trimmed)
			if ((r % 2 == 0)); then arms=(trimmed stock); fi
			for arm in "${arms[@]}"; do
				dir=$root/s-$arm
				rm -rf "$dir"
				emit load "$phase" "$arm" "$r" "$(cut -d' ' -f1-3 /proc/loadavg)"
				if [ "$arm" = stock ]; then
					t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-ast --locked "${flags[@]}")
				else
					t=$(timed env CARGO_TARGET_DIR="$dir" cargo build -p omp-ast "${patches[@]}" "${flags[@]}")
				fi
				cp "$lock" Cargo.lock
				if grep -q 'was not used in the crate graph' "$root/last.log"; then
					echo "error: a [patch] entry was not used" >&2
					return 1
				fi
				emit cold-ast "$phase" "$arm" "$r" $t "$(du -sm "$dir" | cut -f1)"
				timings "$dir/cargo-timings/cargo-timing.html" "$phase" "$arm" "$r"
				for entry in "${archives[@]}"; do
					crate=${entry%%:*}
					libname=${entry#*:}
					lib=$(find "$dir" -path "*/build/$crate-*/out/lib$libname.a" | head -1)
					if [ -z "$lib" ]; then
						echo "error: no lib$libname.a under $dir" >&2
						return 1
					fi
					emit objects "$phase" "$arm" "$r" "$crate" "$(ar t "$lib" | wc -l)" "$(stat -c %s "$lib")"
				done
				rm -rf "$dir"
			done
		done
	done
}

"$phase"
