"""Cold-audit thread scaling: CODIE_THREADS=1,2,4,8,16, CODIE_TIMING=1.

Usage: python -I thread_scaling.py <repo> <out.json> [--runs 3] [--threads 1,2,4,8,16]
Each run deletes <repo>/.codereader/ first. Reports the median parse-stage
time, median in-process total and median wall time per thread count.
"""
import argparse, json, os, re, shutil, statistics, subprocess, time

_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_EXE = "codereader.exe" if os.name == "nt" else "codereader"
CODIE = os.environ.get("CODIE_BIN") or os.path.join(_ROOT, "target", "release", _EXE)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("repo")
    ap.add_argument("out")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--threads", default="1,2,4,8,16")
    a = ap.parse_args()
    repo = os.path.abspath(a.repo)
    res = {}
    for n in [int(x) for x in a.threads.split(",")]:
        runs = []
        for _ in range(a.runs):
            shutil.rmtree(os.path.join(repo, ".codereader"), ignore_errors=True)
            env = dict(os.environ, CODIE_THREADS=str(n), CODIE_TIMING="1")
            t0 = time.perf_counter()
            p = subprocess.run([CODIE, "audit", repo, "--json"], cwd=repo, env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            wall = (time.perf_counter() - t0) * 1000
            err = p.stderr.decode("utf-8", "replace")
            st = {m.group(1).strip(): float(m.group(2))
                  for m in re.finditer(r"\[timing\]\s+(.+?)\s+([\d.]+) ms", err)}
            runs.append({"exit": p.returncode, "wall_ms": round(wall, 1),
                         "parse_ms": st.get("parse + extract"), "total_ms": st.get("TOTAL")})
        res[n] = {"runs": runs,
                  "parse_median": statistics.median(r["parse_ms"] for r in runs),
                  "total_median": statistics.median(r["total_ms"] for r in runs),
                  "wall_median": statistics.median(r["wall_ms"] for r in runs)}
        print(n, res[n]["parse_median"], res[n]["total_median"], res[n]["wall_median"],
              [r["parse_ms"] for r in runs], flush=True)
    json.dump(res, open(a.out, "w"), indent=1)


if __name__ == "__main__":
    main()
