#!/usr/bin/env python3
"""Mechanical checks for the gateway-capabilities research set.

Run from anywhere:  python3 docs/research/gateway-capabilities/evidence/check_docs.py [--refs /private/tmp/omp-research-refs]

Checks (each prints PASS/FAIL lines; exit status 1 if any FAIL):
  1. README links all deliverables 1-20 and every linked file exists.
  2. Every markdown file links back to the index.
  3. Every relative markdown link resolves.
  4. Every capability card (### PREFIX-NN title) has exactly one verbatim decision class.
  5. No em dashes in prose.
  6. Every `path:line` citation into the OMP2 tree points at an existing file and a line inside it;
     when a backticked symbol follows, the symbol occurs in that file.
  7. Every `owner/repo@sha7:path` citation points at a file that exists in the pinned clone.
  8. Mermaid fences are balanced and diagram files carry a CURRENT/PROPOSED/REFERENCE status line.
It checks form and existence, not truth. A passing run does not prove a claim is correct.
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
G = os.path.dirname(HERE)  # docs/research/gateway-capabilities
ROOT = os.path.abspath(os.path.join(G, "..", "..", ".."))
REFS = "/private/tmp/omp-research-refs"
if "--refs" in sys.argv:
    REFS = sys.argv[sys.argv.index("--refs") + 1]

CLASSES = [
    "Already implemented: reuse",
    "Partially implemented: extend",
    "Missing and justified: propose",
    "Suitable as an optional extension",
    "Suitable as an external integration",
    "Requires benchmarking",
    "Requires ADR reconsideration",
    "Unnecessary or redundant",
    "Rejected for security, licensing or operational reasons",
]
DELIVERABLES = {
    1: "01-executive-architectural-assessment.md",
    2: "02-verified-current-state.md",
    3: "03-gateway-capability-matrix.md",
    4: "04-existing-vs-missing-matrix.md",
    5: "05-provider-integration-recommendations.md",
    6: "06-transport-and-protocol-recommendations.md",
    7: "07-abi-independent-extension-assessment.md",
    8: "08-python-runtime-comparative-assessment.md",
    9: "09-single-session-swarm-assessment.md",
    10: "10-local-inference-assessment.md",
    11: "11-code-intelligence-and-scip-assessment.md",
    12: "12-proposed-architecture-diagrams.md",
    13: "13-performance-and-benchmark-plan.md",
    14: "14-security-and-licensing-assessment.md",
    15: "15-prioritized-architectural-backlog.md",
    16: "16-draft-adrs.md",
    17: "17-incremental-implementation-roadmap.md",
    18: "18-rejected-alternatives.md",
    19: "19-source-references.md",
    20: "20-open-questions.md",
}

fails = 0


def fail(msg):
    global fails
    fails += 1
    print("FAIL", msg)


def ok(msg):
    print("PASS", msg)


def md_files():
    out = []
    for dp, _dn, fn in os.walk(G):
        for f in fn:
            if f.endswith(".md"):
                out.append(os.path.join(dp, f))
    return sorted(out)


def read(p):
    with open(p, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def strip_code(text):
    """Remove fenced code blocks (keeps line count by blanking)."""
    out, infence = [], False
    for line in text.split("\n"):
        if line.lstrip().startswith("```"):
            infence = not infence
            out.append("")
            continue
        out.append("" if infence else line)
    return "\n".join(out)


files = md_files()
readme = os.path.join(G, "README.md")

# 1. index links
if not os.path.exists(readme):
    fail("README.md missing")
else:
    rtxt = read(readme)
    miss = []
    for n, name in DELIVERABLES.items():
        if name not in rtxt:
            miss.append(f"{n}:{name} not linked")
        elif not os.path.exists(os.path.join(G, name)):
            miss.append(f"{n}:{name} linked but file missing")
    if miss:
        fail("index deliverables: " + "; ".join(miss))
    else:
        ok("index links all 20 deliverables and the files exist")

# 2. back-links
nolink = []
for p in files:
    if p == readme:
        continue
    if p.endswith("00-owner-brief-verbatim.md"):
        continue  # verbatim copy of the owner's text, left untouched
    txt = read(p)
    if not re.search(r"\]\((\.\./)*README\.md\)", txt):
        nolink.append(os.path.relpath(p, G))
if nolink:
    fail("no back-link to index: " + ", ".join(nolink))
else:
    ok("every markdown file links back to the index")

# 3. relative links resolve
bad = []
for p in files:
    txt = strip_code(read(p))
    for m in re.finditer(r"\]\(([^)\s]+)\)", txt):
        tgt = m.group(1)
        if re.match(r"^(https?:|mailto:|#)", tgt):
            continue
        tgt = tgt.split("#")[0]
        if not tgt:
            continue
        full = os.path.normpath(os.path.join(os.path.dirname(p), tgt))
        if not os.path.exists(full):
            bad.append(f"{os.path.relpath(p, G)} -> {tgt}")
if bad:
    fail("unresolved links: " + "; ".join(bad[:25]) + (" ..." if len(bad) > 25 else ""))
else:
    ok("all relative links resolve")

# 4. capability cards
card_re = re.compile(r"^### ((?:PRV|AUTH|EXT|PY|IPC|SWM|INF|CIX|GWY|OBS|SEC)-\d+)\b.*$", re.M)
cards = 0
badcards = []
for p in files:
    txt = strip_code(read(p))
    ms = list(card_re.finditer(txt))
    for i, m in enumerate(ms):
        cards += 1
        end = ms[i + 1].start() if i + 1 < len(ms) else len(txt)
        nxt = re.search(r"^#{1,3} ", txt[m.end():end], re.M)
        body = txt[m.end(): m.end() + nxt.start()] if nxt else txt[m.end():end]
        decl = re.findall(r"^\*\*Decision class:\*\*\s*(.+?)\s*$", body, re.M)
        if len(decl) != 1:
            badcards.append(f"{os.path.relpath(p, G)}:{m.group(1)} has {len(decl)} decision-class lines")
            continue
        val = decl[0].strip("` ")
        if val not in CLASSES:
            badcards.append(f"{os.path.relpath(p, G)}:{m.group(1)} class not verbatim: {val!r}")
if badcards:
    fail("cards: " + "; ".join(badcards[:30]) + (" ..." if len(badcards) > 30 else ""))
else:
    ok(f"{cards} capability cards each carry exactly one verbatim decision class")

# 5. em dashes
em = []
for p in files:
    if p.endswith("00-owner-brief-verbatim.md"):
        continue
    for i, line in enumerate(read(p).split("\n"), 1):
        if "\u2014" in line:
            em.append(f"{os.path.relpath(p, G)}:{i}")
if em:
    fail(f"em dashes at {len(em)} places: " + ", ".join(em[:15]) + (" ..." if len(em) > 15 else ""))
else:
    ok("no em dashes")

# 6. OMP2 path:line citations
cite_re = re.compile(r"((?:crates|extensions|docs|packages|scripts|vendor|fixtures|tools)/[A-Za-z0-9_./+\-]+\.[A-Za-z0-9]+):(\d+)(?:[-,](\d+))?(?:\s+((?:`[^`]+`)))?")
tot = good = 0
problems = []
cache = {}
for p in files:
    if p.endswith("00-owner-brief-verbatim.md"):
        continue
    txt = read(p)
    for m in cite_re.finditer(txt):
        path, l1, l2, sym = m.group(1), int(m.group(2)), m.group(3), m.group(4)
        tot += 1
        full = os.path.join(ROOT, path)
        if not os.path.isfile(full):
            problems.append(f"{os.path.relpath(p, G)}: missing file {path}")
            continue
        if full not in cache:
            cache[full] = read(full)
        content = cache[full]
        n = content.count("\n") + 1
        if l1 > n or (l2 and int(l2) > n):
            problems.append(f"{os.path.relpath(p, G)}: {path}:{l1} beyond EOF ({n} lines)")
            continue
        if sym:
            tok = sym.strip("`")
            ident = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", tok)
            ident = [x for x in ident if x not in ("fn", "struct", "enum", "impl", "trait", "mod", "const", "static", "type", "pub", "async", "def", "class")]
            if ident and ident[-1] not in content and ident[0] not in content:
                problems.append(f"{os.path.relpath(p, G)}: symbol {tok} not in {path}")
                continue
        good += 1
if problems:
    fail(f"OMP2 citations: {good}/{tot} resolve; first problems: " + "; ".join(problems[:20]) + (" ..." if len(problems) > 20 else ""))
else:
    ok(f"OMP2 citations: {tot} checked, all resolve")

# 7. reference-repo citations
inv_path = os.path.join(REFS, "inventory.json")
ref_re = re.compile(r"\b([A-Za-z0-9_.\-]+/[A-Za-z0-9_.\-]+)@([0-9a-f]{7,40})(?::([A-Za-z0-9_./+\-@\[\]]+))?")
if os.path.exists(inv_path):
    inv = {x["repo"]: x for x in json.load(open(inv_path)) if x.get("sha")}
    rtot = rgood = 0
    rprob = []
    for p in files:
        if p.endswith("00-owner-brief-verbatim.md") or p.endswith("ref-inventory.md"):
            continue
        for m in ref_re.finditer(read(p)):
            repo, sha, path = m.group(1), m.group(2), m.group(3)
            if repo not in inv:
                continue
            rtot += 1
            if not inv[repo]["sha"].startswith(sha):
                rprob.append(f"{os.path.relpath(p, G)}: {repo}@{sha} does not match pinned {inv[repo]['sha'][:7]}")
                continue
            if path:
                path = path.rstrip(".,;:)")
                path = re.sub(r":\d+(-\d+)?$", "", path)
                full = os.path.join(inv[repo]["dir"], path)
                if not os.path.exists(full):
                    rprob.append(f"{os.path.relpath(p, G)}: {repo}@{sha}:{path} not in clone")
                    continue
            rgood += 1
    if rprob:
        fail(f"reference citations: {rgood}/{rtot} resolve; first problems: " + "; ".join(rprob[:20]) + (" ..." if len(rprob) > 20 else ""))
    else:
        ok(f"reference citations: {rtot} checked, all resolve")
else:
    print("SKIP reference citations: inventory.json not found at", inv_path)

# 8. mermaid fences and diagram status
mm = []
for p in files:
    txt = read(p)
    if txt.count("```") % 2:
        mm.append(f"{os.path.relpath(p, G)}: odd number of code fences")
    if os.sep + "diagrams" + os.sep in p:
        if "```mermaid" not in txt:
            mm.append(f"{os.path.relpath(p, G)}: no mermaid block")
        if not re.search(r"\b(CURRENT|PROPOSED|REFERENCE REPOSITORY)\b", txt):
            mm.append(f"{os.path.relpath(p, G)}: no CURRENT/PROPOSED/REFERENCE status")
if mm:
    fail("diagrams/fences: " + "; ".join(mm[:20]))
else:
    ok("code fences balanced; diagrams carry a mermaid block and a status")

print("\nSUMMARY:", "FAILURES" if fails else "ALL CHECKS PASSED", f"({fails} failing)")
sys.exit(1 if fails else 0)
