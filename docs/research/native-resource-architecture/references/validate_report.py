#!/usr/bin/env python3
"""Validate research links/ranges and decisions without mutating production files."""
import json
from pathlib import Path
import re
import subprocess
from urllib.parse import unquote, urlsplit

HERE = Path(__file__).resolve().parents[1]
ROOT = HERE.parents[2]


def main():
    errors = []
    local_count = 0
    source_count = 0
    inline_count = 0
    unverified = set()
    repositories = {"blyzer/oh-my-pi": ROOT}
    cached_sources = {}
    index = Path("/workspace/research-sources/tech/library-evidence/index.json")
    if index.exists():
        for item in json.loads(index.read_text()):
            for entry in item.get("files", []):
                if entry.get("local"):
                    cached_sources[(item["repository"].lower(), item["sha"], entry["path"])] = Path(entry["local"])
    for gitdir in Path("/workspace/research-sources").rglob(".git"):
        if not gitdir.is_dir():
            continue
        run = subprocess.run(["git", "-C", str(gitdir.parent), "remote", "get-url", "origin"],
                             capture_output=True, text=True)
        remote = run.stdout.strip().removesuffix(".git")
        match = re.search(r"github\.com[:/](.+/.+)$", remote)
        if match:
            repositories[match[1].lower()] = gitdir.parent
    for document in sorted(HERE.glob("*.md")):
        text = document.read_text()
        for found in re.finditer(r"(?<![/\w])((?:crates|docs|scripts)/[A-Za-z0-9_./-]+\.(?:rs|md|py|proto|toml))(?::(\d+)(?:[-–](\d+))?)?", text):
            relative, start, end = found.groups()
            path = ROOT / relative
            inline_count += 1
            if not path.is_file():
                # External source rows can explicitly name a foreign crates/ tree.
                foreign = [clone / relative for clone in repositories.values()
                           if clone != ROOT and (clone / relative).is_file()]
                if len(foreign) == 1:
                    path = foreign[0]
            if not path.is_file():
                errors.append(f"{document.name}: missing inline source {relative}")
            elif start:
                length = len(path.read_text().splitlines())
                if not 1 <= int(start) <= int(end or start) <= length:
                    errors.append(f"{document.name}: invalid inline range {found[0]} ({length} lines)")
        targets = re.findall(r"\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)", text)
        targets += re.findall(r"(?m)^\[[^\]]+\]:\s*(\S+)", text)
        for target in targets:
            parts = urlsplit(target)
            if not parts.scheme and parts.path:
                local_count += 1
                if not (document.parent / unquote(parts.path)).exists():
                    errors.append(f"{document.name}: missing relative target {target}")
            match = re.fullmatch(r"/([^/]+/[^/]+)/blob/([0-9a-f]{40})/(.+)", parts.path)
            if parts.netloc != "github.com" or not match:
                continue
            repository, sha, path = match.groups()
            clone = repositories.get(repository.lower())
            cached = cached_sources.get((repository.lower(), sha, unquote(path)))
            if clone is None and cached is None:
                unverified.add(target)
                continue
            if cached is not None:
                source_text = cached.read_text()
            else:
                run = subprocess.run(["git", "-C", str(clone), "show", sha+":"+unquote(path)],
                                     capture_output=True, text=True)
                if run.returncode:
                    errors.append(f"{document.name}: missing pinned source {target}")
                    continue
                source_text = run.stdout
            source_count += 1
            lines = source_text.splitlines()
            anchor = re.fullmatch(r"L(\d+)(?:-L(\d+))?", parts.fragment)
            if anchor and not (1 <= int(anchor[1]) <= int(anchor[2] or anchor[1]) <= len(lines)):
                errors.append(f"{document.name}: invalid source range {target} ({len(lines)} lines)")
    records = json.loads((HERE / "adoption-records.json").read_text())["opportunities"]
    ids = {record["id"] for record in records}
    required = set(records[0])
    for record in records:
        if set(record) != required or not record["acceptance_criteria"]:
            errors.append("Incomplete decision " + record["id"])
        if not (ROOT / record["source_path"]).exists():
            errors.append("Missing decision source " + record["source_path"])
        if any(dependency not in ids for dependency in record["dependencies"]):
            errors.append("Unknown dependency " + record["id"])
    def visit(identifier, active):
        if identifier in active:
            raise ValueError("Decision dependency cycle")
        row = next(row for row in records if row["id"] == identifier)
        for dep in row["dependencies"]:
            visit(dep, active | {identifier})
    for identifier in ids:
        visit(identifier, set())
    answers = (HERE / "08-final-answers.md").read_text()
    numbers = [int(n) for n in re.findall(r"\*\*(\d+)\. ", answers)]
    if numbers != list(range(1, 51)):
        errors.append("Final answers are not exactly 1..50")
    result = {"relative_links_checked": local_count, "pinned_source_targets_checked": source_count,
              "inline_source_paths_checked": inline_count,
              "external_targets_not_verified_by_this_local_checker": sorted(unverified),
              "decision_records": len(records), "fields_per_record": len(required), "errors": errors}
    print(json.dumps(result, indent=2))
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())
