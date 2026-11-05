#!/usr/bin/env python3
"""Tables for docs/research/grammar-bundling.md from the GS lines in raw/*.tsv.

Every raw file is the GS output of one BuildBuddy run (one VM). Build deltas are paired within a
run and repetition, so VM-to-VM speed differences cancel. Usage: analyze.py [raw-dir].
"""

import collections
import statistics
import sys
from pathlib import Path

RAW = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).with_name("raw"))
VARIANTS = ["all", "hot", "hot-wasm", "none"]


def rows():
    for path in sorted(RAW.glob("*.tsv")):
        for line in path.read_text().splitlines():
            if line.startswith("GS\t"):
                yield path.stem, line.split("\t")[1:]


def spread(values, fmt="{:.1f}"):
    values = sorted(values)
    if not values:
        return "n/a"
    med = statistics.median(values)
    if len(values) == 1:
        return fmt.format(med) + " (n=1)"
    return f"{fmt.format(med)} ({fmt.format(values[0])}-{fmt.format(values[-1])}, n={len(values)})"


def build_tables(data, phase, jobs):
    cold = collections.defaultdict(dict)
    timing = collections.defaultdict(dict)
    incr = collections.defaultdict(lambda: collections.defaultdict(list))
    for run, f in data:
        if f[0] == "cold-ast" and f[1] == phase and jobs.get(run) is not None:
            cold[(run, f[3])][f[2]] = (float(f[4]), float(f[5]) + float(f[6]), int(f[7]))
        elif f[0] == "timings" and f[1] == phase:
            timing[(run, f[3])][f[2]] = [float(x) for x in f[4:]]
        elif f[0] in ("noop-ast", "rebuild-ast", "build-kb", "rebuild-kb") and f[1] == phase:
            incr[f[0]][f[2]].append(float(f[4]))
    for jobset in sorted(set(jobs.values())):
        keys = [k for k in cold if jobs[k[0]] == jobset]
        if not keys:
            continue
        command = "cargo build -p omp-app --bin omp" if phase == "omp" else "cargo build -p omp-ast"
        pairs = sum(1 for k in keys if "none" in cold[k] and len(cold[k]) > 1)
        print(f"\n## {phase}, -j {jobset}: cold `{command}` ({len(keys)} run-repetitions, {pairs} with a paired none)\n")
        print("| Variant | Wall s | CPU (user+sys) s | Target MB | Grammar unit time s | Grammar C compile s | Wall delta vs none s | Share of wall |")
        print("|---|---|---|---|---|---|---|---|")
        for v in VARIANTS:
            have = [k for k in keys if v in cold[k]]
            if not have:
                continue
            wall = [cold[k][v][0] for k in have]
            cpu = [cold[k][v][1] for k in have]
            mb = [cold[k][v][2] for k in have]
            g_all = [timing[k][v][4] for k in have if v in timing[k]]
            g_run = [timing[k][v][5] for k in have if v in timing[k]]
            deltas = [cold[k][v][0] - cold[k]["none"][0] for k in have if "none" in cold[k]]
            shares = [(cold[k][v][0] - cold[k]["none"][0]) / cold[k][v][0] * 100 for k in have if "none" in cold[k]]
            print(f"| {v} | {spread(wall)} | {spread(cpu)} | {spread(mb, '{:.0f}')} | {spread(g_all)} | {spread(g_run)} | "
                  f"{spread(deltas) if v != 'none' else '-'} | {spread(shares, '{:.0f}%') if v != 'none' else '-'} |")
    if incr:
        print(f"\n## {phase}: incremental steps after the cold build (wall s)\n")
        print("| Step | " + " | ".join(VARIANTS) + " |")
        print("|---|" + "---|" * len(VARIANTS))
        for step in ("noop-ast", "rebuild-ast", "build-kb", "rebuild-kb"):
            if step in incr:
                print(f"| {step} | " + " | ".join(spread(incr[step][v], "{:.2f}") for v in VARIANTS) + " |")


SUB_CRATES = {"tree-sitter-ocaml", "tree-sitter-php", "tree-sitter-md", "tree-sitter-xml"}


def sub_label(phase):
    return phase.removeprefix("sub-").replace("-j", ", -j ")


def subgrammar_tables(data):
    cold = collections.defaultdict(dict)
    timing = collections.defaultdict(dict)
    members = collections.defaultdict(lambda: collections.defaultdict(set))
    units = collections.defaultdict(dict)
    for run, f in data:
        if not f[1].startswith("sub-"):
            continue
        if f[0] == "cold-ast":
            cold[(f[1], run, f[3])][f[2]] = (float(f[4]), float(f[5]) + float(f[6]))
        elif f[0] == "timings":
            timing[(f[1], run, f[3])][f[2]] = float(f[9])
        elif f[0] == "objects":
            members[f[4]][f[2]].add(int(f[5]))
        elif f[0] == "unit" and f[3] in SUB_CRATES and "(run)" in f[5]:
            units[(f[1], f[3])][f[2]] = float(f[7])
    if not cold:
        return
    print("\n## Unused sub-grammars: cold `cargo build -p omp-ast`, crates as shipped against trimmed\n")
    print("| Profile, jobs | Pairs | Shipped wall s | Trimmed wall s | Wall saved s | Share of wall | CPU saved s | Grammar C compile saved s |")
    print("|---|---|---|---|---|---|---|---|")
    for phase in sorted({k[0] for k in cold}):
        keys = [k for k in cold if k[0] == phase and {"stock", "trimmed"} <= cold[k].keys()]
        stock = [cold[k]["stock"][0] for k in keys]
        trim = [cold[k]["trimmed"][0] for k in keys]
        saved = [cold[k]["stock"][0] - cold[k]["trimmed"][0] for k in keys]
        share = [(cold[k]["stock"][0] - cold[k]["trimmed"][0]) / cold[k]["stock"][0] * 100 for k in keys]
        cpu = [cold[k]["stock"][1] - cold[k]["trimmed"][1] for k in keys]
        crun = [timing[k]["stock"] - timing[k]["trimmed"] for k in keys if {"stock", "trimmed"} <= timing[k].keys()]
        print(f"| {sub_label(phase)} | {len(keys)} | {spread(stock)} | {spread(trim)} | {spread(saved)} | "
              f"{spread(share, '{:.0f}%')} | {spread(cpu)} | {spread(crun)} |")
    print("\nObject files in each crate's archive, every build (a parser.o and a scanner.o per compiled grammar):\n")
    print("| Crate | Shipped | Trimmed |")
    print("|---|---|---|")
    for crate in sorted(members):
        shipped = "/".join(map(str, sorted(members[crate]["stock"])))
        trimmed = "/".join(map(str, sorted(members[crate]["trimmed"])))
        print(f"| {crate} | {shipped} | {trimmed} |")
    print("\nBuild-script run time of each crate in the first repetition (s):\n")
    print("| Profile, jobs | Crate | Shipped | Trimmed |")
    print("|---|---|---|---|")
    for (phase, crate), d in sorted(units.items()):
        print(f"| {sub_label(phase)} | {crate} | {d.get('stock', float('nan')):.2f} | {d.get('trimmed', float('nan')):.2f} |")


def main():
    print("# Grammar bundling: generated tables\n")
    print("Generated by `analyze.py` from `raw/*.tsv` for [`../grammar-bundling.md`](../grammar-bundling.md). Do not edit.")
    data = list(rows())
    jobs = {}
    for path in sorted(RAW.glob("*.tsv")):
        for line in path.read_text().splitlines():
            if line.startswith("host:") and "jobs=" in line:
                jobs[path.stem] = int(line.split("jobs=")[1].split(";")[0])
    for phase in ("dev", "release", "omp"):
        build_tables(data, phase, jobs)
    subgrammar_tables(data)

    relink = collections.defaultdict(lambda: collections.defaultdict(list))
    sizes = {}
    packed = {}
    for run, f in data:
        if f[0] in ("relink", "relink-omp") and f[3]:
            relink[f[0]][f[1]].append(float(f[3]) * 1000)
        elif f[0].startswith("size-"):
            sizes.setdefault((f[0], f[1]), set()).add(tuple(f[2:]))
        elif f[0] == "bytes":
            packed.setdefault(f[1], set()).add(tuple(f[2:]))
    for kind, binary in (("relink", "omp-kb"), ("relink-omp", "omp")):
        if kind in relink:
            print(f"\n## Relink of the dev `{binary}` binary (rustc `-Z time-passes` run_linker, ms)\n")
            print("| " + " | ".join(VARIANTS) + " |")
            print("|" + "---|" * len(VARIANTS))
            print("| " + " | ".join(spread(relink[kind][v], "{:.0f}") for v in VARIANTS) + " |")
    if sizes:
        print("\n## Binary size (bytes; identical across repetitions unless listed twice)\n")
        print("| Binary, profile | Variant | File | .text | .rodata | .data.rel.ro | Grammar entry symbols | Unreferenced sub-grammar symbols |")
        print("|---|---|---|---|---|---|---|---|")
        for (kind, v), values in sorted(sizes.items()):
            for value in sorted(values):
                print(f"| {kind.removeprefix('size-')} | {v} | " + " | ".join(value) + " |")
    if packed:
        print("\n## Release binary bytes, raw and compressed\n")
        print("| Binary | Raw | gzip -9 | zstd -19 |")
        print("|---|---|---|---|")
        for name, values in sorted(packed.items()):
            for value in sorted(values):
                print(f"| {name} | " + " | ".join(value) + " |")

    down = collections.defaultdict(list)
    first = collections.defaultdict(list)
    steady = collections.defaultdict(list)
    for run, f in data:
        if f[0] == "download":
            down[f[1]].append(f)
        elif f[0] == "first":
            first[f[1]].append(f)
        elif f[0] == "steady":
            steady[f[1]].append(f)
    if down:
        print("\n## Download from the stencil-hq/wasm-grammars v1 release (one new HTTP client per fetch)\n")
        print("| Language | .zst bytes | .wasm bytes | HTTP ms | zstd decode ms | SHA-256 ms | write ms | verified |")
        print("|---|---|---|---|---|---|---|---|")
        for lang, fs in sorted(down.items(), key=lambda kv: -int(kv[1][0][4])):
            print(f"| {lang} | {fs[0][3]} | {fs[0][4]} | {spread([float(x[5]) for x in fs], '{:.0f}')} | "
                  f"{spread([float(x[6]) for x in fs], '{:.2f}')} | {spread([float(x[7]) for x in fs], '{:.2f}')} | "
                  f"{spread([float(x[8]) for x in fs], '{:.2f}')} | {sum(x[9] == 'true' for x in fs)}/{len(fs)} |")
    if first:
        print("\n## First use in a fresh process (ms): wasm module compile, store, first parse; native first parse\n")
        print("| Language | .wasm bytes | Sample bytes | Engine | Store | Compile (load_language) | Parser store | set_language | First parse | WASM total | Native first parse | Trees equal |")
        print("|---|---|---|---|---|---|---|---|---|---|---|---|")
        for lang, fs in sorted(first.items(), key=lambda kv: -int(kv[1][0][4])):
            col = lambda i, fmt="{:.1f}": spread([float(x[i]) for x in fs], fmt)
            print(f"| {lang} | {fs[0][4]} | {fs[0][5]} | {col(6, '{:.2f}')} | {col(8, '{:.2f}')} | {col(9)} | {col(10, '{:.2f}')} | "
                  f"{col(11, '{:.2f}')} | {col(12, '{:.2f}')} | {col(13)} | {col(14, '{:.2f}')} | {sum(x[19] == 'true' for x in fs)}/{len(fs)} |")
        totals = [statistics.median(float(x[13]) for x in fs) for fs in first.values()]
        compiles = {lang: statistics.median(float(x[9]) for x in fs) for lang, fs in first.items()}
        sizes_w = {lang: int(fs[0][4]) for lang, fs in first.items()}
        print(f"\nMedian WASM total over languages: {statistics.median(totals):.1f} ms; max {max(totals):.1f} ms. "
              f"Compile ms per MB of module: {spread([compiles[l] / (sizes_w[l] / 1e6) for l in compiles if sizes_w[l] > 1e6])} "
              f"(modules over 1 MB).")
    if steady:
        print("\n## Steady-state parse: one parser reused (per-parse median of each run; range across runs)\n")
        print("| Language | Sample bytes | Parses per side per run | Trees equal | Native us | WASM us | WASM/native | Native MB/s | WASM MB/s | WASM store growth, bytes per byte parsed | Native growth KB |")
        print("|---|---|---|---|---|---|---|---|---|---|---|")
        ratio_med = {lang: statistics.median(float(f[12]) for f in fs) for lang, fs in steady.items()}
        for lang, fs in sorted(steady.items(), key=lambda kv: -ratio_med[kv[0]]):
            size, n = int(fs[0][2]), int(fs[0][3])
            n_med = [float(f[6]) for f in fs]
            w_med = [float(f[9]) for f in fs]
            growth = [(int(f[14]) - int(f[13])) * 1024 / (size * n) for f in fs]
            native_growth = max(int(f[16]) - int(f[15]) for f in fs)
            print(f"| {lang} | {size} | {n} | {sum(f[5] == 'true' for f in fs)}/{len(fs)} | {spread(n_med, '{:.0f}')} | "
                  f"{spread(w_med, '{:.0f}')} | {spread([float(f[12]) for f in fs], '{:.2f}')} | {size / statistics.median(n_med):.1f} | "
                  f"{size / statistics.median(w_med):.1f} | {spread(growth, '{:.1f}')} | {native_growth} |")
        ratios = list(ratio_med.values())
        growths = [statistics.median((int(f[14]) - int(f[13])) * 1024 / (int(f[2]) * int(f[3])) for f in fs) for fs in steady.values()]
        print(f"\nWASM/native median-parse ratio over {len(ratios)} languages: {spread(ratios, '{:.2f}')}. "
              f"WASM store growth over languages: {spread(growths, '{:.1f}')} bytes per byte parsed.")


if __name__ == "__main__":
    main()
