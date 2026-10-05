import sys
import json, re, glob, collections
root = sys.argv[1]  # <projects dir>/<session id>
files = [root + ".jsonl"] + sorted(glob.glob(root + "/subagents/*.jsonl"))
def tl(c):
    if isinstance(c, str): return c
    return "".join(x.get("text", "") for x in c if isinstance(x, dict) and x.get("type") == "text") if isinstance(c, list) else ""
SIGNAL = re.compile(r'(error(\[|:)|\bFAIL\b|panicked|SIGABRT|TIMEOUT|^\s*warning:|-->|Summary|assertion|test result:|failures:|stack overflow|##\[error\])', re.I)
def cat(cmd):
    c = cmd.strip()
    if re.search(r'\b(cargo|just|nextest|rustfmt|clippy)\b', c) and re.search(r'\b(build|check|test|nextest|clippy|fmt|e2e|lint|ci|doc)\b', c): return "verify (cargo/just/nextest)"
    if re.search(r'\b(tail|head|cat|grep|sed -n|rg)\b.*\.(log|txt|out)\b', c) or re.search(r'(\.log|\.txt|\.out)\b.*\|\s*(tail|head|grep|rg)', c): return "reading logs"
    if re.match(r'^(cd [^;&|]+(&&|;)\s*)?git\b', c): return "git"
    if re.search(r'\bgh api\b|curl .*github|get_job_logs', c): return "github/CI"
    if re.match(r'^(cd [^;&|]+(&&|;)\s*)?(rg|grep|egrep|find|fd|ls|tree|wc)\b', c): return "code search/list"
    if re.match(r'^(cd [^;&|]+(&&|;)\s*)?(cat|head|tail|sed -n)\b', c): return "file reading"
    if re.search(r'\b(sleep|until|while|pgrep|ps aux|df -h|du -s)\b', c): return "polling/disk/ps"
    return "other"
agg = collections.defaultdict(lambda: {"calls": 0, "chars": 0, "signal": 0})
tool_cat = collections.Counter()
for path in files:
    uses = {}
    with open(path, errors="replace") as fh:
        for line in fh:
            try: d = json.loads(line)
            except Exception: continue
            m = d.get("message")
            if not isinstance(m, dict) or not isinstance(m.get("content"), list): continue
            for b in m["content"]:
                if not isinstance(b, dict): continue
                if b.get("type") == "tool_use" and b.get("name") == "Bash": uses[b["id"]] = (b.get("input") or {}).get("command", "")
                elif b.get("type") == "tool_result" and b.get("tool_use_id") in uses:
                    k = cat(uses[b["tool_use_id"]]); txt = tl(b.get("content"))
                    a = agg[k]; a["calls"] += 1; a["chars"] += len(txt)
                    a["signal"] += sum(len(l) + 1 for l in txt.splitlines() if SIGNAL.search(l))
tot = sum(a["chars"] for a in agg.values())
print(f"bash results total approx tokens: {tot//4}")
print(f"{'category':32}{'calls':>7}{'tokens':>10}{'share':>7}{'signal%':>8}{'noise tok':>11}")
for k, a in sorted(agg.items(), key=lambda kv: -kv[1]["chars"]):
    sig = a["signal"] / a["chars"] if a["chars"] else 0
    print(f"{k:32}{a['calls']:>7}{a['chars']//4:>10}{a['chars']/tot:>7.0%}{sig:>8.0%}{int(a['chars']*(1-sig))//4:>11}")
