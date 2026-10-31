#!/usr/bin/env python3
"""Generate AST-level symbol indexes via `rust-analyzer symbols`.

Unlike gen-symbols.py (column-0 `pub` text scan), this walks every `.rs` file
through rust-analyzer's structure outline:

  - nested impl methods / trait methods
  - fields, enum variants
  - private and `pub(crate)` items
  - parent-qualified paths (`Kernel::run_turn`)

It is still **not** a workspace SCIP index: no cross-file references, no trait
impl resolution across crates. For that, use gen-scip.sh when analysis succeeds.

Outputs under docs/knowledge-base/:
  ast-lookup.json.gz — name / qualified name → locations (committed; no detail)
  ast-summary.json   — counts by kind/crate
  by-crate-ast/*.jsonl — local full dump (gitignored unless OMP_KB_AST_KEEP_JSONL=1)
"""

from __future__ import annotations

import gzip
import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CRATES = ROOT / "crates"
OUT = ROOT / "docs" / "knowledge-base"
RA = os.environ.get("RUST_ANALYZER", "rust-analyzer")

# Debug-printed StructureNode lines from `rust-analyzer symbols`.
NODE_RE = re.compile(
	r"StructureNode \{\s*"
	r"parent: (?:None|Some\((?P<parent>\d+)\))\s*,\s*"
	r'label: "(?P<label>(?:\\.|[^"\\])*)"\s*,\s*'
	r"navigation_range: (?P<nav_start>\d+)\.\.(?P<nav_end>\d+)\s*,\s*"
	r"node_range: (?P<node_start>\d+)\.\.(?P<node_end>\d+)\s*,\s*"
	r"kind: SymbolKind\((?P<kind>\w+)\)\s*,\s*"
	r'detail: (?:None|Some\("(?P<detail>(?:\\.|[^"\\])*)"\))\s*,\s*'
	r"deprecated: (?P<deprecated>true|false)\s*"
	r"\}"
)

# Full outline kinds rust-analyzer may emit.
ALL_KINDS = {
	"Struct",
	"Enum",
	"Trait",
	"Function",
	"Method",
	"Const",
	"Static",
	"TypeAlias",
	"Impl",
	"Module",
	"Field",
	"Variant",
	"Macro",
	"Union",
	"TraitAlias",
}

# Committed durable index omits Field/Variant noise (still available via
# OMP_KB_AST_FULL=1 for local full dumps).
DEFAULT_KEEP = ALL_KINDS - {"Field", "Variant"}

GENERIC_STRIP = re.compile(r"<[^<>]*>")


def strip_generics(name: str) -> str:
	prev = None
	cur = name
	while prev != cur:
		prev = cur
		cur = GENERIC_STRIP.sub("", cur)
	return cur


def byte_to_line(data: bytes, offset: int) -> int:
	# 1-indexed line number for the byte offset.
	return data.count(b"\n", 0, offset) + 1


def qualify(nodes_by_raw: dict[int, dict], node: dict) -> str:
	parts = [node["label"]]
	# Walk parents using original StructureNode indices stored on the node.
	raw = node.get("raw_parent")
	guard = 0
	while raw is not None and guard < 64:
		parent = nodes_by_raw.get(raw)
		if parent is None:
			break
		# Skip Impl wrappers in the path display: `impl Foo` → use Foo label cleaned
		lab = parent["label"]
		if parent["kind"] == "Impl":
			# labels look like "impl Foo" / "impl Trait for Foo"
			lab = re.sub(r"^impl\s+", "", lab)
			lab = re.sub(r"^.*\s+for\s+", "", lab)
		parts.append(lab)
		raw = parent.get("raw_parent")
		guard += 1
	parts.reverse()
	return "::".join(parts)


def analyze_file(path: Path, keep_kinds: set[str]) -> list[dict]:
	rel = str(path.relative_to(ROOT)).replace("\\", "/")
	crate = path.relative_to(CRATES).parts[0]
	data = path.read_bytes()
	proc = subprocess.run(
		[RA, "symbols"],
		input=data,
		capture_output=True,
		check=False,
	)
	if proc.returncode != 0:
		return []
	# Symbols are Debug-printed to stdout as UTF-8 text.
	out = proc.stdout.decode("utf-8", errors="replace")
	# Parse against the RAW unfiltered node stream so parent indices match RA.
	raw_nodes: list[dict] = []
	for m in NODE_RE.finditer(out):
		kind = m.group("kind")
		label = m.group("label").encode("utf-8").decode("unicode_escape")
		detail = m.group("detail")
		if detail is not None:
			detail = detail.encode("utf-8").decode("unicode_escape")
		parent = m.group("parent")
		raw_nodes.append(
			{
				"raw_index": len(raw_nodes),
				"raw_parent": int(parent) if parent is not None else None,
				"label": label,
				"kind": kind,
				"nav_start": int(m.group("nav_start")),
				"detail": detail,
				"deprecated": m.group("deprecated") == "true",
			}
		)
	by_raw = {n["raw_index"]: n for n in raw_nodes}

	entries: list[dict] = []
	for n in raw_nodes:
		if n["kind"] not in keep_kinds:
			continue
		if n["label"] in {"_", ""}:
			continue
		line = byte_to_line(data, n["nav_start"])
		qname = qualify(by_raw, n)
		entries.append(
			{
				"crate": crate,
				"name": n["label"],
				"qual": qname,
				"kind": n["kind"],
				"path": rel,
				"line": line,
				"detail": n["detail"],
			}
		)
	return entries


def main() -> int:
	keep_kinds = ALL_KINDS if os.environ.get("OMP_KB_AST_FULL") == "1" else DEFAULT_KEEP
	files = [
		p
		for p in CRATES.rglob("*.rs")
		if "tests" not in p.parts and p.is_file()
	]
	files.sort()
	print(f"scanning {len(files)} files with {RA} symbols…", file=sys.stderr)

	all_entries: list[dict] = []
	workers = min(32, max(4, (os.cpu_count() or 8)))
	with ThreadPoolExecutor(max_workers=workers) as pool:
		futs = {pool.submit(analyze_file, p, keep_kinds): p for p in files}
		done = 0
		for fut in as_completed(futs):
			done += 1
			if done % 200 == 0 or done == len(files):
				print(f"  {done}/{len(files)}", file=sys.stderr)
			try:
				all_entries.extend(fut.result())
			except Exception as exc:  # noqa: BLE001 — keep scanning
				print(f"warn: {futs[fut]}: {exc}", file=sys.stderr)

	lookup: dict[str, list] = defaultdict(list)
	by_crate: dict[str, list] = defaultdict(list)
	kinds = Counter()
	for e in all_entries:
		kinds[e["kind"]] += 1
		by_crate[e["crate"]].append(e)
		loc = {
			"crate": e["crate"],
			"kind": e["kind"],
			"path": e["path"],
			"line": e["line"],
			"qual": e["qual"],
			"detail": e["detail"],
		}
		keys = {e["name"], e["qual"]}
		stripped = strip_generics(e["qual"])
		if stripped:
			keys.add(stripped)
		bare = strip_generics(e["name"])
		if bare:
			keys.add(bare)
		for key in keys:
			lookup[key].append(loc)

	OUT.mkdir(parents=True, exist_ok=True)

	# Slim lookup for git: drop detail, dedupe identical locs per key.
	slim: dict[str, list] = {}
	for key, locs in lookup.items():
		seen: set[tuple] = set()
		out_locs: list[dict] = []
		for loc in locs:
			sig = (loc["crate"], loc["kind"], loc["path"], loc["line"], loc["qual"])
			if sig in seen:
				continue
			seen.add(sig)
			out_locs.append(
				{
					"crate": loc["crate"],
					"kind": loc["kind"],
					"path": loc["path"],
					"line": loc["line"],
					"qual": loc["qual"],
				}
			)
		slim[key] = out_locs

	summary = {
		"format": "omp-knowledge-base-ast/v1",
		"method": "rust-analyzer symbols (per-file structure outline)",
		"full": os.environ.get("OMP_KB_AST_FULL") == "1",
		"kept_kinds": sorted(keep_kinds),
		"totals": {
			"symbols": len(all_entries),
			"lookup_keys": len(slim),
			"files": len(files),
			"crates": len(by_crate),
			"by_kind": dict(sorted(kinds.items())),
		},
		"crates": {
			c: {
				"count": len(items),
				"by_kind": dict(Counter(i["kind"] for i in items)),
			}
			for c, items in sorted(by_crate.items())
		},
	}
	(OUT / "ast-summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
	payload = json.dumps(slim, separators=(",", ":"), sort_keys=True).encode("utf-8")
	(OUT / "ast-lookup.json.gz").write_bytes(gzip.compress(payload, compresslevel=9))

	if os.environ.get("OMP_KB_AST_KEEP_JSONL") == "1":
		by_dir = OUT / "by-crate-ast"
		by_dir.mkdir(exist_ok=True)
		for old in by_dir.glob("*.jsonl"):
			old.unlink()
		for crate, items in by_crate.items():
			lines = [json.dumps(e, separators=(",", ":")) for e in items]
			(by_dir / f"{crate}.jsonl").write_text("\n".join(lines) + "\n", encoding="utf-8")

	if os.environ.get("OMP_KB_AST_KEEP_JSON") == "1":
		(OUT / "ast-lookup.json").write_text(
			json.dumps(slim, indent=2, sort_keys=True) + "\n", encoding="utf-8"
		)

	gz_size = (OUT / "ast-lookup.json.gz").stat().st_size
	print(
		f"wrote {len(all_entries)} AST symbols "
		f"({len(slim)} keys, {gz_size} byte gz) → {OUT.relative_to(ROOT)}",
		file=sys.stderr,
	)
	return 0


if __name__ == "__main__":
	raise SystemExit(main())
