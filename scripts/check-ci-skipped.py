#!/usr/bin/env python3
"""Check that ci-skipped.yml mirrors ci.yml.

A pull request that ci.yml's `pull_request.paths` filter keeps out of CI would
never report the required checks. ci-skipped.yml reports them instead, so:

* its `paths-ignore` list must equal ci.yml's `pull_request.paths` list, and
* each of its job `name:` values must equal a job `name:` in ci.yml, byte for
  byte, because a required check is matched by name.

Plain text scanning keeps this free of third-party YAML dependencies.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CI = ROOT / ".github/workflows/ci.yml"
SKIPPED = ROOT / ".github/workflows/ci-skipped.yml"


def unquote(value: str) -> str:
    value = value.split(" #")[0].strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


def block(lines: list[str], header: str, indent: int) -> list[str]:
    """Lines nested under the first `header:` at exactly `indent` spaces."""
    prefix = " " * indent + header + ":"
    for i, line in enumerate(lines):
        if line.rstrip() == prefix or line.startswith(prefix + " "):
            out = []
            for rest in lines[i + 1 :]:
                if rest.strip() and (len(rest) - len(rest.lstrip())) <= indent:
                    break
                out.append(rest)
            return out
    return []


def list_items(lines: list[str], key: str, parent: str) -> list[str]:
    section = block(lines, parent, 2)
    return [
        unquote(line.strip()[2:])
        for line in block(section, key, 4)
        if line.strip().startswith("- ")
    ]


def job_names(lines: list[str]) -> list[str]:
    names = []
    for line in block(lines, "jobs", 0):
        match = re.match(r"^    name: (.+)$", line)
        if match:
            names.append(unquote(match.group(1)))
    return names


def main() -> int:
    ci = CI.read_text().splitlines()
    skipped = SKIPPED.read_text().splitlines()
    ok = True

    paths = list_items(ci, "paths", "pull_request")
    ignore = list_items(skipped, "paths-ignore", "pull_request")
    if not paths or not ignore:
        print("could not read the path lists", file=sys.stderr)
        return 1
    if set(paths) != set(ignore):
        ok = False
        for item in sorted(set(paths) - set(ignore)):
            print(f"in ci.yml paths but not ci-skipped.yml paths-ignore: {item}")
        for item in sorted(set(ignore) - set(paths)):
            print(f"in ci-skipped.yml paths-ignore but not ci.yml paths: {item}")

    ci_names = set(job_names(ci))
    skipped_names = job_names(skipped)
    for name in skipped_names:
        if name not in ci_names:
            ok = False
            print(f"ci-skipped.yml job name has no match in ci.yml: {name!r}")
    if len(skipped_names) != len(set(skipped_names)):
        ok = False
        print("ci-skipped.yml repeats a job name")

    if ok:
        print(f"ok: {len(paths)} paths and {len(skipped_names)} check names match")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
