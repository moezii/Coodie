"""Benchmark `codereader audit --json` on one repository.

Usage: python -I bench.py <name> <repo_path> <out_dir> [--runs N] [--timeout S]

Per repository:
  * N cold runs: `.codereader/` deleted before each run.
  * N warm runs: cache left in place (populated by the last cold run).
  * 1 cold + 1 warm run with CODIE_TIMING=1 (stage breakdown, stderr kept).
Wall time is time.perf_counter() around the subprocess (spawn to exit).
Peak memory is the process's Windows PeakWorkingSet (psutil peak_wset),
polled every 5 ms; the last value read before exit is kept, plus max RSS seen.
peak_wset exists only on Windows; elsewhere only max RSS is meaningful.

The binary under test is $CODIE_BIN, or target/release/codereader in this
checkout.
"""
import argparse, json, os, shutil, statistics, subprocess, sys, time
import psutil

_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_EXE = "codereader.exe" if os.name == "nt" else "codereader"
CODIE = os.environ.get("CODIE_BIN") or os.path.join(_ROOT, "target", "release", _EXE)


def run_once(repo, out_json, env_extra=None, timeout=600):
    env = os.environ.copy()
    env.pop("CODIE_TIMING", None)
    if env_extra:
        env.update(env_extra)
    with open(out_json, "wb") as fo, open(out_json + ".stderr", "wb") as fe:
        t0 = time.perf_counter()
        p = subprocess.Popen([CODIE, "audit", repo, "--json"], cwd=repo,
                             stdout=fo, stderr=fe, env=env)
        try:
            ps = psutil.Process(p.pid)
        except psutil.Error:
            ps = None
        peak_wset = 0
        max_rss = 0
        timed_out = False
        while p.poll() is None:
            if ps is not None:
                try:
                    mi = ps.memory_info()
                    peak_wset = max(peak_wset, getattr(mi, "peak_wset", 0))
                    max_rss = max(max_rss, mi.rss)
                except psutil.Error:
                    pass
            if time.perf_counter() - t0 > timeout:
                p.kill()
                timed_out = True
                break
            time.sleep(0.005)
        p.wait()
        wall = time.perf_counter() - t0
    with open(out_json + ".stderr", "rb") as fe:
        stderr = fe.read().decode("utf-8", "replace")
    return {"wall_ms": round(wall * 1000, 1), "exit": p.returncode,
            "peak_wset_mb": round(peak_wset / 2**20, 1),
            "max_rss_seen_mb": round(max_rss / 2**20, 1),
            "timed_out": timed_out, "stderr": stderr[-4000:]}


def clear_cache(repo):
    d = os.path.join(repo, ".codereader")
    if os.path.isdir(d):
        shutil.rmtree(d)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("name")
    ap.add_argument("repo")
    ap.add_argument("out")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--timeout", type=int, default=600)
    a = ap.parse_args()
    repo = os.path.abspath(a.repo)
    os.makedirs(a.out, exist_ok=True)
    res = {"name": a.name, "repo": repo, "cold": [], "warm": []}

    for i in range(a.runs):
        clear_cache(repo)
        r = run_once(repo, os.path.join(a.out, f"{a.name}_cold{i}.json"), timeout=a.timeout)
        res["cold"].append(r)
        print(a.name, "cold", i, r["wall_ms"], "ms", r["peak_wset_mb"], "MB exit", r["exit"], flush=True)
        if r["exit"] != 0 or r["timed_out"]:
            break
    for i in range(a.runs):
        r = run_once(repo, os.path.join(a.out, f"{a.name}_warm{i}.json"), timeout=a.timeout)
        res["warm"].append(r)
        print(a.name, "warm", i, r["wall_ms"], "ms", r["peak_wset_mb"], "MB exit", r["exit"], flush=True)
        if r["exit"] != 0 or r["timed_out"]:
            break

    clear_cache(repo)
    res["timing_cold"] = run_once(repo, os.path.join(a.out, f"{a.name}_timing_cold.json"),
                                  {"CODIE_TIMING": "1"}, a.timeout)
    res["timing_warm"] = run_once(repo, os.path.join(a.out, f"{a.name}_timing_warm.json"),
                                  {"CODIE_TIMING": "1"}, a.timeout)

    for k in ("cold", "warm"):
        ws = [r["wall_ms"] for r in res[k]]
        res[k + "_median_ms"] = statistics.median(ws) if ws else None
        res[k + "_peak_mb_max"] = max((r["peak_wset_mb"] for r in res[k]), default=None)
    with open(os.path.join(a.out, f"{a.name}_bench.json"), "w", encoding="utf-8") as f:
        json.dump(res, f, indent=1)
    print(a.name, "cold median", res["cold_median_ms"], "warm median", res["warm_median_ms"])


if __name__ == "__main__":
    main()
