#!/usr/bin/env python3
"""Read pinned historical research/implementation objects; never check them out."""
import json
import subprocess

BASE = "fda1315052b8b4074f9ea5acc842b2cc0b08b2b2"
BRANCHES = {
    "research/grammar-bundling-harness-omp": "b04b268acd7afa2d7e64b3343f1e86fde3f528c8",
    "research/omp2-native-hybrid-verification": "63a7b37aefb6e5744d80dddc4b681f19e320e573",
    "feat/memory-search-async": "c1707cff16d20f3840a561ff507f1338e95446bb",
    "adw-native-executor": "a163b31bdf46d57c1f9486ac837341ea072cfeb5",
    "adw-verification-policy": "417c0596b47233da3201106e51b4ac3949f032bc",
    "cursor/capabilities-p0-conformance-632b": "e6aba31ea72c606bbca195033295c50430e2e74b",
    "cursor/capabilities-p1-skill-retain-632b": "1f397712635c0bb23abc37a291d1c43bd13a9ed7",
}


def output(*args):
    return subprocess.check_output(["git", *args], text=True)


if __name__ == "__main__":
    records = []
    for name, sha in BRANCHES.items():
        ancestor = subprocess.run(["git", "merge-base", "--is-ancestor", sha, BASE], capture_output=True)
        records.append({"branch": name, "sha": sha, "ancestor_of_baseline": ancestor.returncode == 0,
                        "tip_summary": output("show", "--stat", "--oneline", "--no-renames", sha),
                        "tip_patch_excerpt": output("show", "--format=short", "--no-ext-diff", sha)[:12000],
                        "status_note": "Non-ancestry does not distinguish open work from squash/cherry-pick; current implementation conclusions use baseline source."})
    print(json.dumps({"baseline": BASE, "branches": records}, indent=2))
