# Benchmark scripts

Scripts used for [BENCHMARKS.md](../BENCHMARKS.md).

| Script | Purpose |
|---|---|
| `bench.py` | 5 cold runs (cache deleted), 5 warm runs, then one of each with `CODIE_TIMING=1`. Records wall time, exit code and peak memory; writes `<name>_bench.json` and each run's output |
| `summarize.py` | Prints the Markdown tables for one output directory |
| `sample_dead.py` | Seeded random sample of `Dead` symbols with every occurrence of each name, for manual review |
| `verify_all.py` | Every `Dead` symbol with every occurrence of its name outside its definition. `--tests N` also prints N (or all) tests reported unable to fail |
| `thread_scaling.py` | Cold audits at `CODIE_THREADS=1,2,4,8,16`; median parse, total and wall time per thread count |

Requirements: Python 3 with `psutil`, git, and a release build. The scripts use
`$CODIE_BIN` if set, otherwise `target/release/codereader`. Peak working set is
Windows-only; other platforms record max RSS.

## Running

```sh
cargo build --release

pin() { mkdir -p "bench/repos/$1" && git -C "bench/repos/$1" init -q &&
        git -C "bench/repos/$1" fetch -q --depth 1 "$2" "$3" &&
        git -C "bench/repos/$1" checkout -q FETCH_HEAD; }
pin reth      https://github.com/paradigmxyz/reth              685039caf7b8114d2b246c237bc1f6128ca15e22
pin Stockfish https://github.com/official-stockfish/Stockfish  7cdd5e89a39fe3071b67d91c7bb07d712044bf76
pin lc0       https://github.com/LeelaChessZero/lc0            fb94c44af4ece84791bac1ed03dd4bbe5dbd3d49

python -m venv bench/venv
bench/venv/Scripts/python -m pip install psutil     # bench/venv/bin/python elsewhere

# Benchmark Codie on a copy so the cache in your checkout is left alone.
git archive --format=tar HEAD --prefix=codie/ | tar -x -C bench/repos

PY=bench/venv/Scripts/python
for t in "codie bench/repos/codie" "stockfish bench/repos/Stockfish" \
         "lc0 bench/repos/lc0" "reth bench/repos/reth"; do
  set -- $t; $PY -I bench/bench.py "$1" "$2" bench/raw
done
$PY -I bench/summarize.py bench/raw
$PY -I bench/verify_all.py bench/raw/lc0_cold0.json bench/repos/lc0 --tests 15
$PY -I bench/thread_scaling.py bench/repos/reth bench/raw/threads_reth.json --runs 3
```

`bench/repos/`, `bench/raw/` and `bench/venv/` are gitignored.

`bench.py` deletes `<repo>/.codereader/` before every cold run, so do not point
it at a checkout whose cache you want to keep.

Run the whole set twice and use the second pass. Right after cloning, the first
pass is slowed by first-touch disk reads and, on Windows, antivirus scanning.
On the initial import it was 1.5 to 2.5x slower, and one cold Reth run took
100.8 s.
