#!/usr/bin/env python3
"""Compare rust-analyzer's semantic answers with what the Claude history paid for the same questions.

Run from the repository root, with rust-analyzer on PATH:
    lsp_eval.py <data dir> <out dir>

A. find-references: for each usage-style `rg <identifier>` search recorded in data/usages.json,
   ask rust-analyzer (workspace/symbol, then textDocument/references) and compare the size of the
   answer, and the files it names, with what rg returned.
B. symbol slices: size of the exact function/method bodies (textDocument/documentSymbol) against the
   average `sed -n` range read in data/history_stats.json.
"""
import json, os, queue, random, statistics as st, subprocess, sys, threading, time
from pathlib import Path
from urllib.parse import quote, unquote, urlparse

data, out = Path(sys.argv[1]), Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
root = Path.cwd().resolve()
LOAD_TIMEOUT = int(os.environ.get("RA_LOAD_TIMEOUT", "3000"))


class Lsp:
    def __init__(self):
        self.p = subprocess.Popen(["rust-analyzer"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=open(out / "rust-analyzer.stderr.log", "wb"))
        self.q, self.n, self.quiescent, self.status = queue.Queue(), 0, False, None
        self.pending = {}
        threading.Thread(target=self._read, daemon=True).start()

    def _send(self, msg):
        body = json.dumps(msg).encode()
        self.p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body); self.p.stdin.flush()

    def _read(self):
        f = self.p.stdout
        while True:
            length = None
            while True:
                line = f.readline()
                if not line: return
                line = line.strip()
                if not line: break
                if line.lower().startswith(b"content-length:"): length = int(line.split(b":")[1])
            msg = json.loads(f.read(length))
            if "method" in msg and "id" in msg:      # server -> client request
                result = [None] * len(msg.get("params", {}).get("items", [])) if msg["method"] == "workspace/configuration" else None
                self._send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
            elif "method" in msg:
                if msg["method"] == "experimental/serverStatus":
                    self.status = msg["params"]; self.quiescent = bool(msg["params"].get("quiescent"))
            elif "id" in msg:
                self.pending[msg["id"]] = msg

    def request(self, method, params, timeout=600):
        self.n += 1; i = self.n
        self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        end = time.time() + timeout
        while i not in self.pending:
            if time.time() > end: raise TimeoutError(method)
            time.sleep(0.01)
        msg = self.pending.pop(i)
        if "error" in msg: raise RuntimeError(msg["error"])
        return msg["result"]

    def notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})


def uri(path): return "file://" + quote(str(path))
def rel(u): return str(Path(unquote(urlparse(u).path)).resolve().relative_to(root))
def tokens(text): return len(text) // 4


t0 = time.time()
ls = Lsp()
init = ls.request("initialize", {
    "processId": os.getpid(), "rootUri": uri(root), "workspaceFolders": [{"uri": uri(root), "name": "omp"}],
    "capabilities": {"experimental": {"serverStatusNotification": True},
                     "window": {"workDoneProgress": True},
                     "workspace": {"configuration": True, "symbol": {}},
                     "textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": True}}},
    "initializationOptions": {"checkOnSave": False, "workspace": {"symbol": {"search": {"kind": "allSymbols", "limit": 512}}}, "cachePriming": {"enable": True},
                              "cargo": {"buildScripts": {"enable": True}}, "procMacro": {"enable": True}},
}, timeout=300)
ls.notify("initialized", {})
deadline = time.time() + LOAD_TIMEOUT
seen_loading = False
while time.time() < deadline:
    if ls.status and not ls.quiescent: seen_loading = True
    if ls.quiescent and (seen_loading or time.time() - t0 > 120): break
    time.sleep(2)
load_s = round(time.time() - t0)
server = init.get("serverInfo", {})
print(f"rust-analyzer {server} ready after {load_s}s status={ls.status}", flush=True)

# ---- A. find-references vs rg usage searches
usages = json.load(open(data / "usages.json"))
rows = []
for rec in usages:
    cur_all = {f for f in rec["files"] if (root / f).exists()}
    cur = {f for f in cur_all if f.endswith(".rs")}
    if len(cur_all) < 3 or not cur: continue
    t1 = time.time()
    try:
        syms = [s for s in (ls.request("workspace/symbol", {"query": rec["symbol"]}) or []) if s["name"] == rec["symbol"]]
    except Exception as e:
        rows.append({"symbol": rec["symbol"], "error": repr(e)}); continue
    cands = []
    for s in syms[:6]:
        loc = s["location"]
        try:
            refs = ls.request("textDocument/references", {"textDocument": {"uri": loc["uri"]}, "position": loc["range"]["start"],
                                                          "context": {"includeDeclaration": False}}) or []
        except Exception as e:
            continue
        lines = sorted({f"{rel(r['uri'])}:{r['range']['start']['line'] + 1}" for r in refs})
        grouped = {}
        for l in lines: grouped.setdefault(l.rsplit(":", 1)[0], 0); grouped[l.rsplit(":", 1)[0]] += 1
        files = set(grouped)
        cands.append({"kind": s.get("kind"), "container": s.get("containerName"), "refs": len(lines), "files": len(files),
                      "tokens_lines": tokens("\n".join(lines)), "tokens_files": tokens("\n".join(f"{f} ({n})" for f, n in grouped.items())),
                      "recall": round(len(cur & files) / len(cur), 2), "precision": round(len(files & cur) / len(files), 2) if files else None})
    best = max(cands, key=lambda c: c["recall"]) if cands else None
    rows.append({"symbol": rec["symbol"], "calls": rec["calls"], "hist_tokens": rec["avg_tokens"], "hist_files": len(cur_all), "hist_rs_files": len(cur),
                 "candidates": len(syms), "best": best, "first": cands[0] if cands else None, "secs": round(time.time() - t1, 2)})
json.dump(rows, open(out / "references.json", "w"), indent=0)
ok = [r for r in rows if r.get("best")]
uniq = [r for r in ok if r["candidates"] == 1]
def summary(label, sel, key):
    if not sel: return f"[{label}] n=0"
    g = lambda r, f: r[key][f]
    return (f"[{label}] n={len(sel)} | rg tokens median {st.median(r['hist_tokens'] for r in sel)} "
            f"| RA 'path:line' tokens median {st.median(g(r, 'tokens_lines') for r in sel)} "
            f"| RA grouped-by-file tokens median {st.median(g(r, 'tokens_files') for r in sel)} "
            f"| recall of rg files median {st.median(g(r, 'recall') for r in sel)}, >=0.5 in {sum(1 for r in sel if g(r, 'recall') >= 0.5)}/{len(sel)}, ==0 in {sum(1 for r in sel if g(r, 'recall') == 0)}/{len(sel)} "
            f"| median query {st.median(r['secs'] for r in sel)}s")

# ---- B. symbol-level slices vs ranged reads
random.seed(7)
rs = sorted(p for p in (root / "crates").glob("*/src/**/*.rs") if p.stat().st_size > 4000)
random.shuffle(rs)
slices, whole = [], []
for path in rs[:120]:
    try:
        ds = ls.request("textDocument/documentSymbol", {"textDocument": {"uri": uri(path)}}, timeout=120) or []
    except Exception:
        continue
    lines = path.read_text(errors="replace").splitlines()
    whole.append(tokens("\n".join(lines)))
    def walk(items):
        for it in items:
            if it.get("kind") in (6, 12) and "range" in it:
                a, b = it["range"]["start"]["line"], it["range"]["end"]["line"]
                slices.append(tokens("\n".join(lines[a:b + 1])))
            walk(it.get("children") or [])
    walk(ds)
hist = json.load(open(data / "history_stats.json"))
try:
    peak_rss_mb = int(next(l for l in open(f"/proc/{ls.p.pid}/status") if l.startswith("VmHWM")).split()[1]) // 1024
except Exception:
    peak_rss_mb = "?"
lines_out = [f"rust-analyzer {server}; workspace load {load_s}s; rust-analyzer peak RSS {peak_rss_mb} MB",
             f"A. references, {len(rows)} usage searches asked, {len(ok)} answered, {len(uniq)} with a unique symbol",
             summary("best candidate", ok, "best"), summary("first candidate", [r for r in ok if r["first"]], "first"),
             summary("unique symbol", uniq, "best")]
if slices:
    q = st.quantiles(slices, n=10)
    lines_out += [f"B. fn/method bodies in {len(whole)} sampled files: n={len(slices)} tokens median {st.median(slices)} mean {st.mean(slices):.0f} p90 {q[8]:.0f}; "
                  f"whole file median {st.median(whole)}; history `sed -n` range read avg {hist['sed_n_range_reads']['avg_tokens']}, whole-file cat avg {hist['cat_whole_file']['avg_tokens']}"]
(out / "summary.txt").write_text("\n".join(lines_out) + "\n"); print("\n".join(lines_out))
try: ls.request("shutdown", None, timeout=30); ls.notify("exit", None)
except Exception: pass
