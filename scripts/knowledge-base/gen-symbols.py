#!/usr/bin/env python3
"""Generate docs/knowledge-base symbol indexes from the workspace source tree.

This is a column-0 textual scan of `pub struct|enum|trait|type|fn|const|static`
(module-level API surface). It is not a rustc/rust-analyzer AST. Prefer LSP for
exact resolution; use this index for durable offline "where is X?" memory.

Outputs (under docs/knowledge-base/):
  summary.json   — totals, domains, per-crate counts
  lookup.json    — symbol name → [{crate, kind, path, line}, ...]
  by-crate/*.jsonl — one JSON object per public symbol, per crate
"""

from __future__ import annotations

import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CRATES = ROOT / "crates"
OUT = ROOT / "docs" / "knowledge-base"

# Matches module-level pubs, including `pub async fn` / `pub const unsafe fn`.
PAT = re.compile(
	r"^(?:pub(?:\([^)]*\))?\s+)"
	r"(?:(?:async|const|unsafe|extern\s+\"[^\"]+\")\s+)*"
	r"(?P<kind>struct|enum|trait|type|fn|const|static)\s+"
	r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
)
MOD_PAT = re.compile(r"^(?P<vis>pub\s+)?mod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
USE_PAT = re.compile(r"^pub\s+use\s+(?P<body>[^;]+);")

DOMAINS = {
	"state_authority": ["journal", "dom", "vocab", "session"],
	"control_plane": ["con", "agent", "driver"],
	"inference": ["ai", "catalog", "scribe"],
	"tools_env": ["tool", "tools", "env", "envd", "shell", "shell-builtins"],
	"presentation": ["tui", "chat", "app", "gui", "macros"],
	"extensions": ["ext", "py", "py-link"],
	"transport": ["proto", "rpc", "serve", "sdk", "http"],
	"build_exec": [
		"exec",
		"exec-local",
		"exec-reapi",
		"exec-buildbuddy",
		"exec-ssh",
	],
}


def scan() -> tuple[list[dict], dict, dict]:
	index: list[dict] = []
	modules: dict[str, list] = defaultdict(list)
	reexports: dict[str, list] = defaultdict(list)

	for crate_dir in sorted(CRATES.iterdir()):
		src = crate_dir / "src"
		if not src.is_dir():
			continue
		crate = crate_dir.name
		for path in src.rglob("*.rs"):
			if "tests" in path.parts:
				continue
			rel = str(path.relative_to(ROOT)).replace("\\", "/")
			is_lib = path.name == "lib.rs"
			text = path.read_text(encoding="utf-8", errors="replace")
			for i, line in enumerate(text.splitlines(), 1):
				if line.lstrip().startswith("//"):
					continue
				if not line.startswith("pub"):
					if is_lib:
						m = MOD_PAT.match(line)
						if m:
							modules[crate].append(
								{
									"name": m.group("name"),
									"line": i,
									"public": bool(m.group("vis")),
									"path": rel,
								}
							)
					continue
				m = PAT.match(line)
				if m:
					index.append(
						{
							"crate": crate,
							"name": m.group("name"),
							"kind": m.group("kind"),
							"path": rel,
							"line": i,
						}
					)
					continue
				if is_lib:
					m = MOD_PAT.match(line)
					if m:
						modules[crate].append(
							{
								"name": m.group("name"),
								"line": i,
								"public": True,
								"path": rel,
							}
						)
						continue
					m = USE_PAT.match(line)
					if m:
						reexports[crate].append(
							{
								"line": i,
								"body": m.group("body").strip(),
								"path": rel,
							}
						)
	return index, dict(modules), dict(reexports)


def main() -> int:
	index, modules, reexports = scan()
	lookup: dict[str, list] = defaultdict(list)
	by_crate: dict[str, list] = defaultdict(list)
	crate_counts: dict[str, dict] = {}

	for e in index:
		lookup[e["name"]].append(
			{
				"crate": e["crate"],
				"kind": e["kind"],
				"path": e["path"],
				"line": e["line"],
			}
		)
		by_crate[e["crate"]].append(e)

	for crate, items in by_crate.items():
		kinds = Counter(x["kind"] for x in items)
		crate_counts[crate] = {"count": len(items), "by_kind": dict(kinds)}

	OUT.mkdir(parents=True, exist_ok=True)
	by_dir = OUT / "by-crate"
	by_dir.mkdir(exist_ok=True)
	# wipe stale crate files
	for old in by_dir.glob("*.jsonl"):
		old.unlink()

	summary = {
		"format": "omp-knowledge-base-symbols/v1",
		"method": (
			"column-0 pub struct|enum|trait|type|fn|const|static "
			"(module-level API surface), not full rustc AST"
		),
		"totals": {
			"symbols": len(index),
			"crates": len(by_crate),
			"by_kind": {
				k: sum(1 for e in index if e["kind"] == k)
				for k in (
					"struct",
					"enum",
					"trait",
					"type",
					"fn",
					"const",
					"static",
				)
			},
		},
		"domains": DOMAINS,
		"crates": dict(sorted(crate_counts.items())),
		"modules": modules,
		"reexports": reexports,
	}

	(OUT / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
	(OUT / "lookup.json").write_text(
		json.dumps(lookup, indent=2, sort_keys=True) + "\n", encoding="utf-8"
	)
	for crate, items in by_crate.items():
		lines = [json.dumps(e, separators=(",", ":")) for e in items]
		(by_dir / f"{crate}.jsonl").write_text("\n".join(lines) + "\n", encoding="utf-8")

	print(
		f"wrote {len(index)} symbols across {len(by_crate)} crates → {OUT.relative_to(ROOT)}",
		file=sys.stderr,
	)
	return 0


if __name__ == "__main__":
	raise SystemExit(main())
