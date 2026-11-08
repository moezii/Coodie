"""List EVERY symbol an audit reports Dead, plus every word-boundary
occurrence of its short name anywhere in the repository (all text files,
including docs and vendored code; .git and .codereader skipped), excluding
the symbol's own definition span. Each entry is then judged by hand.

Usage: python -I verify_all.py <audit.json> <repo> [--tests N] [--seed 42]
--tests N also prints N random "tests that cannot fail" (all if fewer), with
their source, for manual review.
"""
import argparse, json, os, random, re

SKIP = {".git", ".codereader"}


def load(repo):
    files = []
    for d, dirs, fs in os.walk(repo):
        dirs[:] = [x for x in dirs if x not in SKIP]
        for f in fs:
            p = os.path.join(d, f)
            try:
                with open(p, "rb") as fh:
                    raw = fh.read()
            except OSError:
                continue
            if b"\0" in raw[:8192]:
                continue
            files.append((os.path.relpath(p, repo).replace("\\", "/"),
                          raw.decode("utf-8", "replace").splitlines()))
    return files


def short(name):
    n = re.sub(r"<[^<>]*>", "", name)
    n = re.sub(r"<[^<>]*>", "", n)
    parts = [w for w in re.split(r"[^A-Za-z0-9_]", n.split("::")[-1]) if w]
    return parts[-1] if parts else name


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("audit")
    ap.add_argument("repo")
    ap.add_argument("--tests", type=int, default=0)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--max", type=int, default=12)
    a = ap.parse_args()
    d = json.load(open(a.audit, encoding="utf-8"))
    dead = sorted((u for u in d["coverage"]["unaccounted"] if u["classification"] == "Dead"),
                  key=lambda u: (u["file"], u["line_range"]["start"]))
    files = load(a.repo)
    zero = 0
    print(f"# {len(dead)} symbols reported Dead")
    for u in dead:
        w = short(u["name"])
        rx = re.compile(r"(?<![A-Za-z0-9_])" + re.escape(w) + r"(?![A-Za-z0-9_])")
        s, e = u["line_range"]["start"], u["line_range"]["end"]
        hits = [(p, i + 1, t.strip()) for p, ls in files for i, t in enumerate(ls)
                if rx.search(t) and not (p == u["file"] and s <= i + 1 <= e)]
        if not hits:
            zero += 1
        print(f"\n=== {u['name']} [{u['file']}:{s}-{e}] word={w!r} other_hits={len(hits)}")
        for p, i, t in hits[: a.max]:
            print(f"    {p}:{i}: {t[:150]}")
        if len(hits) > a.max:
            print(f"    ... {len(hits) - a.max} more")
    print(f"\n# {zero} of {len(dead)} have zero occurrences outside their own definition")
    if a.tests:
        tf = sorted(d["tests"]["assertion_free_tests"], key=lambda x: (x["file"], x["line_range"]["start"]))
        pick = tf if len(tf) <= a.tests else random.Random(a.seed).sample(tf, a.tests)
        print(f"\n# {len(tf)} tests reported unable to fail; showing {len(pick)}")
        src = dict(files)
        for t in pick:
            s, e = t["line_range"]["start"], t["line_range"]["end"]
            print(f"\n--- {t['name']} [{t['file']}:{s}-{e}]")
            ls = src.get(t["file"], [])
            for i in range(max(0, s - 3), min(len(ls), e, s + 40)):
                print(f"    {i + 1}: {ls[i][:150]}")


if __name__ == "__main__":
    main()
