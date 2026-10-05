#!/usr/bin/env python3
"""Check the single-workflow CI gate.

CI is one workflow, ci.yml. A pull request is never filtered out of it by a
path filter (a filtered workflow reports nothing, so its required checks stay
pending). Instead the `changes` job matches the pull request's files against
.github/ci-paths.txt and the other jobs skip themselves, which a required check
counts as passing. This script verifies the pieces that must agree:

* ci.yml's `push.paths` list equals .github/ci-paths.txt;
* ci.yml's `pull_request` trigger carries no `paths` or `paths-ignore` filter;
* every required status check in .github/rulesets/omp2-protected.json is the
  `name:` of a job in ci.yml, because a required check is matched by name.

Plain text scanning keeps this free of third-party YAML dependencies.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CI = ROOT / ".github/workflows/ci.yml"
PATHS = ROOT / ".github/ci-paths.txt"
RULESET = ROOT / ".github/rulesets/omp2-protected.json"


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


def job_names(lines: list[str]) -> list[str]:
    names = []
    for line in block(lines, "jobs", 0):
        match = re.match(r"^    name: (.+)$", line)
        if match:
            names.append(unquote(match.group(1)))
    return names


def main() -> int:
    ci = CI.read_text().splitlines()
    on = block(ci, "on", 0)
    ok = True

    push = [
        unquote(line.strip()[2:])
        for line in block(block(on, "push", 2), "paths", 4)
        if line.strip().startswith("- ")
    ]
    listed = [
        line.strip()
        for line in PATHS.read_text().splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    if not push or not listed:
        print("could not read the path lists", file=sys.stderr)
        return 1
    if set(push) != set(listed):
        ok = False
        for item in sorted(set(push) - set(listed)):
            print(f"in ci.yml push.paths but not .github/ci-paths.txt: {item}")
        for item in sorted(set(listed) - set(push)):
            print(f"in .github/ci-paths.txt but not ci.yml push.paths: {item}")

    pull_request = block(on, "pull_request", 2)
    for key in ("paths", "paths-ignore"):
        if block(pull_request, key, 4):
            ok = False
            print(f"ci.yml pull_request must not filter on `{key}`: required checks would stay pending")

    names = set(job_names(ci))
    required = [
        rule["context"]
        for entry in json.loads(RULESET.read_text())["rules"]
        if entry["type"] == "required_status_checks"
        for rule in entry["parameters"]["required_status_checks"]
    ]
    for context in required:
        if context not in names:
            ok = False
            print(f"required check has no job name in ci.yml: {context!r}")

    if ok:
        print(f"ok: {len(listed)} paths match, {len(required)} required checks are ci.yml jobs")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
