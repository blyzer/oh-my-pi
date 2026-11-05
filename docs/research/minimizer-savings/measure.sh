#!/usr/bin/env bash
# Rebuilds tables.md: `docs/research/minimizer-savings/measure.sh > docs/research/minimizer-savings/tables.md`
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
work=${OMP_SAVINGS_WORK:-${TMPDIR:-/tmp}/omp-minimizer-savings}
mkdir -p "$work/corpus"

fetch() {
	local file=$work/$1 url=$2 expected=$3 actual
	[ -f "$file" ] || curl -sSfL -o "$file" "$url"
	actual=$( (sha256sum "$file" 2>/dev/null || shasum -a 256 "$file") | cut -d' ' -f1)
	if [ "$actual" != "$expected" ]; then
		echo "error: $file has sha256 $actual, expected $expected" >&2
		exit 1
	fi
}

fetch o200k.json \
	https://huggingface.co/Xenova/gpt-4o/resolve/7956d98f2a83b2751a98ea7136fdf7fe6cf54e69/tokenizer.json \
	43a3ad4618a6a938f8c2614154ea10c31ad53a62d5683ae0b0e6133575cef07e
fetch claude-legacy.json \
	https://huggingface.co/Xenova/claude-tokenizer/resolve/cae688821ea05490de49a6d3faa36468a4672fad/tokenizer.json \
	c241737df24b4e7f7c9af4fdcee29a0ca903dcb288a8b753bc346a3092911767
tar -xzf "$here/captures.tar.gz" -C "$work/corpus"
cp "$here/manifest.tsv" "$work/corpus/"
cargo run --profile release-dev --locked -p omp-minimizer --example savings -- \
	"$work/corpus" o200k="$work/o200k.json" claude-legacy="$work/claude-legacy.json" "$@"
