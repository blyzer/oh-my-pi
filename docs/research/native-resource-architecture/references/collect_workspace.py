#!/usr/bin/env python3
"""Declared workspace dependency graph, not a claim all features are activated."""
import json
from pathlib import Path
import subprocess


if __name__ == "__main__":
    root = Path.cwd().resolve()
    argv = ["cargo", "metadata", "--offline", "--locked", "--no-deps", "--format-version", "1"]
    data = json.loads(subprocess.check_output(argv, text=True))
    names = {p["name"] for p in data["packages"]}
    packages = []
    edges = []
    for package in sorted(data["packages"], key=lambda p: p["name"]):
        packages.append({"name": package["name"], "version": package["version"],
                         "manifest": str(Path(package["manifest_path"]).relative_to(root)),
                         "declared_features": package["features"],
                         "targets": [{"name": target["name"], "kind": target["kind"],
                                      "crate_types": target["crate_types"]} for target in package["targets"]]})
        for dep in package["dependencies"]:
            if dep["name"] in names and dep.get("path"):
                edges.append({"from": package["name"], "to": dep["name"],
                              "alias": dep.get("rename"), "kind": dep.get("kind") or "normal",
                              "optional": dep["optional"], "target": dep.get("target"),
                              "uses_default_features": dep["uses_default_features"],
                              "declared_features": dep["features"]})
    print(json.dumps({"pin": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                      "argv": argv, "scope": "Declared internal dependency edges, including optional/target/test edges; not runtime residency or a resolved active-feature graph",
                      "packages": packages, "edges": edges}, indent=2))
