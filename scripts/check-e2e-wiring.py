#!/usr/bin/env python3
"""Check that every end-to-end proof is wired.

AGENTS.md: every proof in crates/e2e/tests is gated in CI, and ci.yml and
`just e2e` stay in step. Nothing enforced that, so a proof can exist and run
nowhere, or run in CI and not from `just e2e`.

For each test target in crates/e2e/tests (`<name>.rs`, or `<name>/main.rs`)
this requires:

* `--test <name>` in a recipe that `just e2e` reaches, so the local suite runs it;
* `--test <name>` in a CI source (.github/workflows/ci.yml or
  scripts/ci-macos-objective.sh), so a pull request runs it.

It also refuses a `--test <name>` in either place that names no target, because
nextest would only fail on it when that job runs.

`EXEMPT` names a proof that is deliberately outside the rule, with the reason.
It is empty: P8 is wired, and only its metric schema test gates.

Plain text scanning keeps this free of third-party dependencies.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TESTS = Path("crates/e2e/tests")
JUSTFILE = Path("justfile")
CI_SOURCES = (Path(".github/workflows/ci.yml"), Path("scripts/ci-macos-objective.sh"))
ENTRY_RECIPE = "e2e"

# Proof name -> why it is not required in `just e2e` and CI.
EXEMPT: dict[str, str] = {}

TEST_ARG = re.compile(r"--test[ =]+([A-Za-z0-9_]+)")
RECIPE_HEADER = re.compile(r"^([A-Za-z0-9_][A-Za-z0-9_-]*)(?:\s+[^:=#]*?)?\s*:(?!=)\s*(.*)$")


def targets(root: Path) -> set[str]:
    """Return the test targets cargo discovers under crates/e2e/tests."""
    directory = root / TESTS
    names = {path.stem for path in directory.glob("*.rs")}
    names |= {path.parent.name for path in directory.glob("*/main.rs")}
    return names


def strip_comments(text: str) -> str:
    """Drop full-line comments, so prose that names a test does not count."""
    return "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))


def named_tests(text: str) -> set[str]:
    """Return every `--test <name>` argument in `text`."""
    return set(TEST_ARG.findall(strip_comments(text)))


def recipes(justfile: str) -> dict[str, tuple[list[str], str]]:
    """Map each recipe to its dependencies and its body text."""
    found: dict[str, tuple[list[str], str]] = {}
    current: str | None = None
    body: list[str] = []
    deps: list[str] = []

    def close() -> None:
        if current is not None:
            found[current] = (deps, "\n".join(body))

    for line in justfile.splitlines():
        if line[:1] in (" ", "\t"):
            if current is not None:
                body.append(line)
            continue
        if not line.strip():
            continue
        stripped = line.strip()
        if stripped.startswith(("#", "[", "set ", "alias ", "import ", "mod ")) or ":=" in line:
            continue
        header = RECIPE_HEADER.match(line)
        if header is None:
            continue
        close()
        current = header.group(1)
        deps = header.group(2).split()
        body = []
    close()
    return found


def reachable(found: dict[str, tuple[list[str], str]], entry: str) -> set[str]:
    """Return `entry` and every recipe it depends on, transitively."""
    seen: set[str] = set()
    pending = [entry]
    while pending:
        name = pending.pop()
        if name in seen or name not in found:
            continue
        seen.add(name)
        pending.extend(found[name][0])
    return seen


def check(root: Path) -> list[str]:
    """Return one message per wiring problem; empty means every proof is wired."""
    problems: list[str] = []
    present = targets(root)
    for name, reason in EXEMPT.items():
        if not reason.strip():
            problems.append(f"EXEMPT[{name!r}] needs a reason")
        if name not in present:
            problems.append(f"EXEMPT[{name!r}] names no test target in {TESTS}")

    justfile = (root / JUSTFILE).read_text(encoding="utf-8")
    parsed = recipes(justfile)
    if ENTRY_RECIPE not in parsed:
        return problems + [f"{JUSTFILE}: no `{ENTRY_RECIPE}` recipe to start from"]
    in_just = set()
    for name in reachable(parsed, ENTRY_RECIPE):
        in_just |= named_tests(parsed[name][1])
    in_ci: set[str] = set()
    for source in CI_SOURCES:
        in_ci |= named_tests((root / source).read_text(encoding="utf-8"))

    for name in sorted(present - EXEMPT.keys()):
        if name not in in_just:
            problems.append(
                f"{TESTS}/{name}: `just {ENTRY_RECIPE}` never runs it; add `--test {name}` "
                f"to a recipe that `{ENTRY_RECIPE}` depends on"
            )
        if name not in in_ci:
            sources = " or ".join(str(source) for source in CI_SOURCES)
            problems.append(
                f"{TESTS}/{name}: no CI job runs it; add `--test {name}` to {sources}"
            )
    for label, named in (("just", in_just), ("CI", in_ci)):
        for name in sorted(named - present):
            problems.append(f"{label} runs `--test {name}`, which is no target in {TESTS}")
    return problems


def main() -> int:
    problems = check(ROOT)
    if problems:
        print("e2e wiring check failed:", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        return 1
    print(f"e2e wiring: {len(targets(ROOT))} proofs, each run by `just {ENTRY_RECIPE}` and by CI")
    return 0


if __name__ == "__main__":
    sys.exit(main())
