"""Pick a reproducible random sample of symbols Codie classified as Dead and
show every word-boundary occurrence of the symbol's short name in the repo.

Usage: python -I sample_dead.py <audit.json> <repo> [--n 10] [--seed 42]
The classification of each sample (truly unreferenced vs false positive) is
done by hand from this output and recorded in RESULTS.md.
"""
import argparse, json, os, random, re

EXTS = None  # all text files
SKIP_DIRS = {".git", ".codereader", "target", "node_modules"}


def short(name):
    n = name.split("::")[-1]
    n = re.sub(r"<.*", "", n)
    return n


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("audit")
    ap.add_argument("repo")
    ap.add_argument("--n", type=int, default=10)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--max-lines", type=int, default=10)
    a = ap.parse_args()
    d = json.load(open(a.audit, encoding="utf-8"))
    dead = [u for u in d["coverage"]["unaccounted"] if u["classification"] == "Dead"]
    dead.sort(key=lambda u: u["node_id"])
    rng = random.Random(a.seed)
    sample = rng.sample(dead, min(a.n, len(dead)))
    files = []
    for root, dirs, fs in os.walk(a.repo):
        dirs[:] = [x for x in dirs if x not in SKIP_DIRS]
        for f in fs:
            files.append(os.path.join(root, f))
    print(f"{len(dead)} dead symbols; sample of {len(sample)} (seed {a.seed})")
    for u in sample:
        nm = short(u["name"])
        pat = re.compile(r"(?<![A-Za-z0-9_])" + re.escape(nm) + r"(?![A-Za-z0-9_])")
        hits = []
        for p in files:
            try:
                with open(p, encoding="utf-8", errors="strict") as fh:
                    for i, line in enumerate(fh, 1):
                        if pat.search(line):
                            hits.append(f"{os.path.relpath(p, a.repo)}:{i}: {line.strip()[:150]}")
            except (UnicodeDecodeError, OSError):
                continue
        print(f"\n### {u['name']}  ({u['file']}:{u['line_range']['start']}-{u['line_range']['end']})  occurrences={len(hits)}")
        for h in hits[: a.max_lines]:
            print("   ", h)
        if len(hits) > a.max_lines:
            print(f"    ... {len(hits) - a.max_lines} more")


if __name__ == "__main__":
    main()
