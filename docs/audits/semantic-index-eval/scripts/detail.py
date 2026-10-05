import sys
import json, re, glob, collections
root = sys.argv[1]  # <projects dir>/<session id>
files = [root + ".jsonl"] + sorted(glob.glob(root + "/subagents/*.jsonl"))
def tl(c):
    if isinstance(c, str): return c
    return "".join(x.get("text", "") for x in c if isinstance(x, dict) and x.get("type") == "text") if isinstance(c, list) else ""
fr = collections.defaultdict(lambda: [0, 0]); gt = collections.defaultdict(lambda: [0, 0])
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
                if b.get("type") == "tool_use" and b.get("name") == "Bash": uses[b["id"]] = (b.get("input") or {}).get("command", "").strip()
                elif b.get("type") == "tool_result" and b.get("tool_use_id") in uses:
                    c = re.sub(r'^cd [^;&|]+(&&|;)\s*', '', uses[b["tool_use_id"]]); n = len(tl(b.get("content")))
                    if re.match(r'^(cat|head|tail|sed -n)\b', c):
                        k = ("sed -n range" if c.startswith("sed -n") else "head/tail" if re.match(r'^(head|tail)\b', c) else
                             "cat whole file" if re.match(r'^cat\s+[^|;&<>]+$', c) else "cat | pipeline")
                        fr[k][0] += 1; fr[k][1] += n
                    elif re.match(r'^git\b', c):
                        m2 = re.match(r'^git\s+(?:-C\s+\S+\s+)?([a-z-]+)', c); k = m2.group(1) if m2 else "git?"
                        gt[k][0] += 1; gt[k][1] += n
print("FILE READING (Bash)"); 
for k, (n, ch) in sorted(fr.items(), key=lambda kv: -kv[1][1]): print(f"  {k:18}{n:>6} calls {ch//4:>9} tokens  avg {ch//4//max(n,1)}")
print("GIT");
for k, (n, ch) in sorted(gt.items(), key=lambda kv: -kv[1][1])[:8]: print(f"  {k:18}{n:>6} calls {ch//4:>9} tokens  avg {ch//4//max(n,1)}")
