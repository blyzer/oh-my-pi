#!/usr/bin/env python3
"""Check remaining pinned GitHub links without vendoring third-party code."""
import base64
import hashlib
import json
import re
import subprocess
from urllib.parse import unquote, urlsplit, quote


if __name__ == "__main__":
    initial = subprocess.run(["python3", "docs/research/native-resource-architecture/references/validate_report.py"],
                             check=True, capture_output=True, text=True)
    targets = json.loads(initial.stdout)["external_targets_not_verified_by_this_local_checker"]
    records = []
    for target in targets:
        parts = urlsplit(target)
        match = re.fullmatch(r"/([^/]+/[^/]+)/blob/([0-9a-f]{40})/(.+)", parts.path)
        repo, sha, path = match.groups()
        endpoint = "repos/" + repo + "/contents/" + quote(unquote(path), safe="/") + "?ref=" + sha
        run = subprocess.run(["gh", "api", endpoint], capture_output=True, text=True, timeout=40)
        row = {"target": target, "argv": ["gh", "api", endpoint], "exit": run.returncode}
        if run.returncode:
            row["status"] = "unverified lookup failure"
        else:
            item = json.loads(run.stdout)
            data = base64.b64decode(item["content"])
            text = data.decode()
            lines = text.splitlines()
            anchor = re.fullmatch(r"L(\d+)(?:-L(\d+))?", parts.fragment)
            row.update({"bytes": len(data), "lines": len(lines), "source_sha256": hashlib.sha256(data).hexdigest(),
                        "git_blob_sha": item["sha"], "status": "checked"})
            if anchor and not (1 <= int(anchor[1]) <= int(anchor[2] or anchor[1]) <= len(lines)):
                row["status"] = "invalid range"
        records.append(row)
    print(json.dumps(records, indent=2))
    raise SystemExit(any(row["status"] != "checked" for row in records))
