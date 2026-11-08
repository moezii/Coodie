"""Print Markdown tables from a pass directory written by bench.py.

Usage: python -I summarize.py <pass_dir>
"""
import json, os, re, sys

NAMES = [("codie", "Codie (self)", "Rust"), ("stockfish", "Stockfish", "C++"),
         ("lc0", "lc0", "C++"), ("reth", "Reth", "Rust")]


def stages(stderr):
    out = {}
    for m in re.finditer(r"\[timing\]\s+(.+?)\s+([\d.]+) ms", stderr):
        out[m.group(1).strip()] = float(m.group(2))
    return out


def main():
    d = sys.argv[1]
    print("| Repo | Lang | Files | Symbols | Cold median | Warm median | Speedup | Peak WS cold | Peak WS warm |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    rows = {}
    for key, label, lang in NAMES:
        p = os.path.join(d, f"{key}_bench.json")
        if not os.path.exists(p):
            continue
        b = json.load(open(p, encoding="utf-8"))
        a = json.load(open(os.path.join(d, f"{key}_cold0.json"), encoding="utf-8"))
        rows[key] = b
        cm, wm = b["cold_median_ms"], b["warm_median_ms"]
        print(f"| {label} | {lang} | {a['files']} | {a['symbols']} | {cm:.1f} ms | {wm:.1f} ms | "
              f"{cm / wm:.1f}x | {b['cold_peak_mb_max']} MB | {b['warm_peak_mb_max']} MB |")
    print("\n### Stage timing (CODIE_TIMING=1, one cold + one warm run, ms)\n")
    names = ["walk", "parse + extract", "graph", "reachability", "coverage + tests + score", "TOTAL"]
    print("| Repo | Run | " + " | ".join(names) + " | Wall (same run) |")
    print("|---|---|" + "---:|" * (len(names) + 1))
    for key, label, _ in NAMES:
        if key not in rows:
            continue
        for run in ("timing_cold", "timing_warm"):
            r = rows[key][run]
            s = stages(r["stderr"])
            print(f"| {label} | {run[7:]} | " + " | ".join(f"{s.get(n, float('nan')):.1f}" for n in names)
                  + f" | {r['wall_ms']:.1f} |")
    print("\n### Raw runs (wall ms / peak working set MB)\n")
    for key, label, _ in NAMES:
        if key not in rows:
            continue
        b = rows[key]
        for k in ("cold", "warm"):
            runs = ", ".join(f"{r['wall_ms']:.1f}/{r['peak_wset_mb']}" for r in b[k])
            bad = [r for r in b[k] if r["exit"] != 0 or r["timed_out"]]
            print(f"- {label} {k}: {runs}" + (f"  (FAILURES: {len(bad)})" if bad else ""))


if __name__ == "__main__":
    main()
