#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import deque
from pathlib import Path

ALL_EXACT = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rust-toolchain"}
ALL_PREFIXES = (
    ".cargo/",
    ".config/",
    "fixtures/",
    "examples/",
    "extensions/",
    "tools/",
    "docs/py/",
)
ALL_CONFIG_FILES = {"rustfmt.toml", "clippy.toml", "deny.toml", "about.toml"}


def run(cmd: list[str], cwd: Path) -> str:
    done = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if done.returncode != 0:
        sys.exit(f"{' '.join(cmd)} failed: {done.stderr.strip()[:400]}")
    return done.stdout


def load_metadata(root: Path, path: str | None) -> dict:
    if path:
        return json.loads(Path(path).read_text())
    return json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked"], root))


def changed_files(root: Path, base: str, head: str) -> list[str]:
    out = run(["git", "diff", "--name-only", "--no-renames", f"{base}...{head}"], root)
    return [line for line in out.splitlines() if line]


def workspace_packages(metadata: dict) -> dict[str, dict]:
    members = set(metadata["workspace_members"])
    root = Path(metadata["workspace_root"])
    packages = {}
    for package in metadata["packages"]:
        if package["id"] in members:
            directory = Path(package["manifest_path"]).parent
            packages[package["id"]] = {
                "name": package["name"],
                "dir": directory.relative_to(root).as_posix(),
            }
    return packages


def reverse_dependencies(metadata: dict, packages: dict[str, dict]) -> dict[str, set[str]]:
    reverse: dict[str, set[str]] = {pid: set() for pid in packages}
    for node in metadata.get("resolve", {}).get("nodes", []):
        if node["id"] not in packages:
            continue
        for dep in node.get("deps", []):
            if dep["pkg"] in packages:
                reverse[dep["pkg"]].add(node["id"])
    return reverse


def file_read_edges(root: Path, packages: dict[str, dict]) -> dict[str, set[str]]:
    directories = {pid: Path(info["dir"]).name for pid, info in packages.items()}
    edges: dict[str, set[str]] = {pid: set() for pid in packages}
    for pid, info in packages.items():
        sources = [p for p in (root / info["dir"]).rglob("*.rs") if "target" not in p.parts]
        text = "\n".join(p.read_text(errors="ignore") for p in sources)
        for other, name in directories.items():
            if other == pid:
                continue
            escaped = re.escape(name)
            patterns = (
                r"\.\./(?:\.\./)*" + escaped + r"(?:/|\"|\b)",
                r"\"\.\.\"\s*,\s*\"" + escaped + r"\"",
                r"\.join\(\"\.\.\"\)(?:\.join\(\"\.\.\"\))*\.join\(\"" + escaped + r"\"\)",
                r"crates/" + escaped + r"\b",
            )
            if any(re.search(pattern, text) for pattern in patterns):
                edges[other].add(pid)
    return edges


def triggers_everything(path: str, package_dirs: list[str]) -> bool:
    if any(path == d or path.startswith(d + "/") for d in package_dirs):
        return False
    if path in ALL_EXACT or path in ALL_CONFIG_FILES:
        return True
    return path.startswith(ALL_PREFIXES)


def affected(metadata: dict, files: list[str], root: Path, exclude: set[str]) -> tuple[str, list[str]]:
    packages = workspace_packages(metadata)
    package_dirs = [info["dir"] for info in packages.values()]
    everything = sorted(info["name"] for info in packages.values() if info["name"] not in exclude)
    if any(triggers_everything(path, package_dirs) for path in files):
        return "all", everything

    owners = sorted(packages.items(), key=lambda item: len(item[1]["dir"]), reverse=True)
    seeds = set()
    for path in files:
        for pid, info in owners:
            if path == info["dir"] or path.startswith(info["dir"] + "/"):
                seeds.add(pid)
                break
    if not seeds:
        return "none", []

    dependents = reverse_dependencies(metadata, packages)
    for pid, readers in file_read_edges(root, packages).items():
        dependents[pid] |= readers
    reached = set(seeds)
    queue = deque(seeds)
    while queue:
        for nxt in dependents[queue.popleft()]:
            if nxt not in reached:
                reached.add(nxt)
                queue.append(nxt)
    names = sorted(packages[pid]["name"] for pid in reached if packages[pid]["name"] not in exclude)
    return ("subset" if names else "none"), names


def main() -> int:
    parser = argparse.ArgumentParser(description="List the workspace crates a change can affect.")
    parser.add_argument("--base", default="origin/omp2")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--exclude", action="append", default=[])
    parser.add_argument("--metadata", help="read cargo metadata JSON from this file")
    parser.add_argument("--files", help="read the changed paths, one per line, from this file")
    parser.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    parser.add_argument("--github-output", help="append scope= and args= lines to this file")
    options = parser.parse_args()

    root = Path(options.root)
    metadata = load_metadata(root, options.metadata)
    if options.files:
        files = [line for line in Path(options.files).read_text().splitlines() if line]
    else:
        files = changed_files(root, options.base, options.head)
    scope, names = affected(metadata, files, root, set(options.exclude))
    args = " ".join(f"-p {name}" for name in names) if scope == "subset" else ""

    print(f"scope={scope} changed_files={len(files)} packages={len(names)}")
    for name in names:
        print(name)
    if options.github_output:
        with open(options.github_output, "a") as handle:
            handle.write(f"scope={scope}\nargs={args}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
