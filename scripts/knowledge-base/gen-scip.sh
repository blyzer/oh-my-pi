#!/usr/bin/env bash
# Generate a workspace SCIP index with rust-analyzer (cross-file refs / impl resolution).
#
# This is the heavy tier. Prefer gen-ast-symbols.py for day-to-day durable memory;
# use this when you need references, trait impls across crates, and external symbols.
#
# Requirements:
#   - rust-analyzer on PATH (pinned toolchain)
#   - optional: Sourcegraph `scip` CLI to print/stats the index
#   - workspace should typecheck under LLVM for best results
#     (Cranelift-only member builds can confuse some analysis paths)
#
# Usage:
#   scripts/knowledge-base/gen-scip.sh
#   scripts/knowledge-base/gen-scip.sh /path/to/out.scip
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$ROOT/docs/knowledge-base/index.scip}"
mkdir -p "$(dirname "$OUT")"

# Exclude vendored / dependency sources from the written index.
# Full workspace load still happens; expect tens of minutes on omp.
exec rust-analyzer scip "$ROOT" \
	--output "$OUT" \
	--exclude-vendored-libraries \
	--num-threads "${SCIP_THREADS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"
