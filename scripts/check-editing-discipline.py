#!/usr/bin/env python3

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

FORMS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("sed in place", re.compile(r"\bsed\s+(?:-[^\s-]*\s+)*(?:-i\b|--in-place\b)")),
    ("perl in place", re.compile(r"\bperl\s+(?:-\w+\s+)*-\w*[pi]i?\b.*-e")),
    ("interpreter heredoc rewriting a file", re.compile(r"\b(?:python|perl|ruby|node)[0-9.]*\s+-\s*<<")),
    ("ed or ex script", re.compile(r"^\s*(?:ed|ex)\s+-s\b|\|\s*(?:ed|ex)\s+-s?\b")),
    ("awk or tee rewriting a tracked path", re.compile(r"\b(?:awk|tee)\b[^|]*>\s*[\"']?(?:\\$\{?\w+\}?|[.~]?/?[\w./-]+\.(?:rs|py|ts|md|toml|ya?ml|json|sh))")),
)

ALLOW_MARKER = re.compile(r"editing-discipline-allow:\s*\S")
PROHIBITED_FENCE = re.compile(r"^\s*(?:\`{3,}|~{3,})\s*text\s+prohibited\s*\$")
FENCE = re.compile(r"^\s*(?:\`{3,}|~{3,})")
RULE_HEADING = "### Editing Discipline (CRITICAL)"
HEADING = re.compile(r"^#{2,3} ")
AUDITED_SUFFIXES = {".md", ".toml", ".ts"}


def audited_surfaces() -> list[Path]:
    paths: list[Path] = [ROOT / "AGENTS.md"]
    for root in (ROOT / "docs", ROOT / ".omp", ROOT / ".claude"):
        if root.is_dir():
            paths += (p for p in root.rglob("*") if p.suffix in AUDITED_SUFFIXES and p.is_file())
    paths += sorted((ROOT / "crates").glob("*/README.md"))
    return [p for p in paths if p.is_file()]


def recipes_in(path: Path) -> list[tuple[int, str, str]]:
    findings: list[tuple[int, str, str]] = []
    inside_exempt_fence = False
    inside_rule_section = False
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    for number, line in enumerate(lines, start=1):
        if line.strip() == RULE_HEADING:
            inside_rule_section = True
            continue
        if inside_rule_section:
            if HEADING.match(line):
                inside_rule_section = False
            else:
                continue
        if inside_exempt_fence:
            inside_exempt_fence = not FENCE.match(line)
            continue
        if PROHIBITED_FENCE.match(line):
            inside_exempt_fence = True
            continue
        preceding = lines[number - 2] if number > 1 else ""
        if ALLOW_MARKER.search(line) or ALLOW_MARKER.search(preceding):
            continue
        findings += [
            (number, name, line.strip()) for name, pattern in FORMS if pattern.search(line)
        ]
    return findings


def main() -> int:
    total = 0
    for path in audited_surfaces():
        for number, name, text in recipes_in(path):
            total += 1
            print(f"{path.relative_to(ROOT)}:{number}: {name}: {text}")
    if total:
        print(
            f"\n{total} patch-script recipe(s) in repo text. AGENTS.md 'Editing Discipline' "
            "binds every prompt and skill body authored here: rewrite the example to use "
            "edit, ast_edit or lsp, or mark the line 'editing-discipline-allow: <reason>' "
            "when it documents what not to do.",
            file=sys.stderr,
        )
        return 1
    print("editing discipline: no patch-script recipes in repo text")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
