"""Extract usage-style identifier searches (rg/grep with >=3 result files) from a Claude session
transcript set. Usage: usages.py <projects dir>/<session id> > data/usages.json
Reads <root>.jsonl and <root>/subagents/*.jsonl. Output has only symbol names, repo-relative file
paths and token counts, never transcript text."""
import glob, json, re, sys
root = sys.argv[1]
files = [root + ".jsonl"] + sorted(glob.glob(root + "/subagents/*.jsonl"))
pat = re.compile(r"""(?:^|[;&|(]\s*|&&\s*)(?:rg|grep|egrep)\s+((?:-[\w-]+(?:=\S+)?\s+(?:\d+\s+)?)*)(['"]?)([^'"\s]+)\2""")
ident = re.compile(r'^[A-Za-z_][A-Za-z0-9_]{4,}$')
pathre = re.compile(r'^(?:\./)?((?:[\w.-]+/)*[\w.-]+\.(?:rs|py|toml|proto|md|kdl|json|ts|txt))(?:[:\-]\d+[:\-]|:|$)', re.M)
uses, recs = {}, {}
for path in files:
    with open(path, errors="replace") as fh:
        for line in fh:
            try: d = json.loads(line)
            except Exception: continue
            m = d.get("message")
            if not isinstance(m, dict) or not isinstance(m.get("content"), list): continue
            for b in m["content"]:
                if not isinstance(b, dict): continue
                if b.get("type") == "tool_use" and b.get("name") == "Bash":
                    uses[b["id"]] = (b.get("input") or {}).get("command", "")
                elif b.get("type") == "tool_result" and b.get("tool_use_id") in uses:
                    mt = pat.search(uses[b["tool_use_id"]])
                    if not mt or not ident.match(mt.group(3)): continue
                    c = b.get("content")
                    txt = c if isinstance(c, str) else "".join(x.get("text", "") for x in c if isinstance(x, dict))
                    fs = {f for f in pathre.findall(txt) if f.startswith(("crates/", "docs/", "scripts/", "tools/"))}
                    if len(fs) < 3: continue
                    r = recs.setdefault(mt.group(3), {"tokens": [], "files": set()})
                    r["tokens"].append(len(txt) // 4); r["files"] |= fs
out = sorted(({"symbol": s, "calls": len(r["tokens"]), "avg_tokens": sum(r["tokens"]) // len(r["tokens"]),
               "files": sorted(r["files"])} for s, r in recs.items()), key=lambda r: -r["calls"])
json.dump(out, sys.stdout)
