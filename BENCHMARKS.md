# Benchmarks

Speed, memory and accuracy of `codereader audit` on public repositories at
pinned commits. The scripts are in [`bench/`](bench/README.md).

## Machine

| | |
|---|---|
| OS | Windows 11 Pro (build 26200), x86_64 |
| CPU | AMD Ryzen 7 6800HS, 8 cores / 16 threads |
| RAM | 31.3 GiB |
| Build | `cargo +stable build --release`, rustc 1.95.0 (59807616e 2026-04-14), `x86_64-pc-windows-msvc` |
| Codie | `codereader 0.1.0` at commit `5e9b242` |
| Date | 2026-11-08 |

## Method

- Command: `codereader audit <repo> --json`, run from inside the repository,
  stdout to a file.
- Cold runs delete `<repo>/.codereader/` first; warm runs keep it. 5 runs of
  each, median reported.
- Wall time is measured from process spawn to exit (`time.perf_counter()`
  around `Popen`).
- Peak memory is the Windows `PeakWorkingSet`, polled every 5 ms, maximum over
  the 5 runs.
- Stage timings come from one extra cold and one extra warm run with
  `CODIE_TIMING=1`.
- Default thread count (one per logical CPU, 16 here) unless stated otherwise.
- Fresh copies of the targets, no cache. The whole set was run twice and the
  second pass is reported. The two passes agree within 2%.

## Targets

Shallow clones without submodules.

| Repository | Commit | Tracked files |
|---|---|---:|
| [Reth](https://github.com/paradigmxyz/reth) | `685039caf7b8114d2b246c237bc1f6128ca15e22` | 1 926 |
| [Stockfish](https://github.com/official-stockfish/Stockfish) | `7cdd5e89a39fe3071b67d91c7bb07d712044bf76` | 119 |
| [lc0](https://github.com/LeelaChessZero/lc0) | `fb94c44af4ece84791bac1ed03dd4bbe5dbd3d49` | 398 |
| Codie (`git archive` of `5e9b242`) | `5e9b242` | 117 walked |

## Results

| Repository | Language | Files | Symbols | Cold | Warm | Speedup | Peak mem cold | Peak mem warm |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Reth | Rust | 1 705 | 23 744 | 4 063 ms | 1 495 ms | 2.7x | 308.8 MB | 269.3 MB |
| lc0 | C++ | 329 | 3 668 | 554 ms | 168 ms | 3.3x | 66.7 MB | 46.4 MB |
| Stockfish | C++ | 87 | 1 462 | 256 ms | 63 ms | 4.0x | 36.6 MB | 23.4 MB |
| Codie | Rust | 117 | 1 117 | 237 ms | 79 ms | 3.0x | 36.5 MB | 22.1 MB |

Files and symbols are what Codie walked and extracted. Vendored code (Reth's
`libmdbx`, lc0's `third_party/`) is listed in the report but not audited.

No run crashed, hung or exited non-zero. Each repository produced
byte-identical JSON in all 24 of its runs (cold, warm and timing, both passes).
Cache size on disk: Reth 68 MB, lc0 7.9 MB, Stockfish 2.5 MB.

### Previous build

Same machine and commits. The previous build was the initial import, built
with rustc 1.98.0 for `x86_64-pc-windows-gnu`, so part of the difference is the
toolchain.

| Repository | Cold, before to now | Warm, before to now |
|---|---:|---:|
| Reth | 19 321 to 4 063 ms (4.8x) | 5 301 to 1 495 ms (3.5x) |
| lc0 | 3 243 to 554 ms (5.9x) | 230 to 168 ms (1.4x) |
| Stockfish | 817 to 256 ms (3.2x) | 69 to 63 ms |
| Codie | 614 to 237 ms | 74 to 79 ms |

Codie itself grew from 109 to 117 files and from 957 to 1 117 symbols between
the two builds. Peak memory went up because files are now parsed in parallel
(Reth cold 285.5 to 308.8 MB, Codie 24.0 to 36.5 MB).

## Stage timings

`CODIE_TIMING=1`, in ms:

| Repository | Run | Walk | Parse + extract | Graph | Reachability | Coverage + tests + score | Total |
|---|---|---:|---:|---:|---:|---:|---:|
| Reth | cold | 154 | 2 687 | 606 | 152 | 253 | 3 891 |
| Reth | warm | 151 | 190 | 588 | 153 | 252 | 1 378 |
| lc0 | cold | 19 | 409 | 57 | 9 | 18 | 521 |
| lc0 | warm | 19 | 31 | 54 | 9 | 17 | 137 |
| Stockfish | cold | 7 | 188 | 15 | 2 | 7 | 222 |
| Stockfish | warm | 7 | 13 | 15 | 3 | 6 | 48 |
| Codie | cold | 13 | 178 | 16 | 4 | 12 | 224 |
| Codie | warm | 12 | 14 | 13 | 4 | 12 | 57 |

Parsing is 69% of a cold Reth audit and 79-85% on the smaller repositories.
Graph and coverage are not cached; on warm Reth they take 840 ms, 61% of the
run. They still grow faster than linearly: 25 ms at 1 117 symbols, 71 ms at
3 668 and 840 ms at 23 744 (6.5x the symbols from lc0 to Reth for 11.8x the
time). Another 140-170 ms of each Reth run falls outside the staged pipeline.

## Thread scaling

Cold Reth, `CODIE_THREADS=1,2,4,8,16`, median of 3, cache deleted before each
run (`bench/thread_scaling.py`):

| Threads | Parse + extract | Speedup | In-process total | Wall |
|---:|---:|---:|---:|---:|
| 1 | 19 994 ms | 1.00x | 21 367 ms | 21 505 ms |
| 2 | 10 596 ms | 1.89x | 11 870 ms | 12 013 ms |
| 4 | 5 939 ms | 3.37x | 7 148 ms | 7 298 ms |
| 8 | 3 639 ms | 5.49x | 4 857 ms | 5 007 ms |
| 16 | 2 638 ms | 7.58x | 3 839 ms | 4 012 ms |

Spread between runs is under 2%. With one thread, parsing is slower than the
old serial parser (20.0 s against 15.9 s on the old toolchain) because the
extractors now do more work.

## Synthetic scaling

Generated Python repositories, ten functions per file, each file calling the
next. Measured on an earlier single-threaded build and not re-run.

| Files | Symbols | Cold | Warm |
|---:|---:|---:|---:|
| 11 | 100 | 177 ms | 21 ms |
| 51 | 500 | 344 ms | 26 ms |
| 201 | 2 000 | 1 143 ms | 69 ms |
| 801 | 8 000 | 4 334 ms | 297 ms |

## Accuracy

Every symbol reported dead was checked by hand with `bench/verify_all.py`,
which lists every whole-word occurrence of the name in every text file of the
repository (docs, scripts, build files and vendored code included) outside the
symbol's own definition.

| | Reth | lc0 | Stockfish | Codie |
|---|---|---|---|---|
| Verdict | FINDINGS (95%) | INCONCLUSIVE (36%) | INCONCLUSIVE (37%) | CLEAN (99%) |
| Score | 88.5 (B) | 41.0 (F) | 47.6 (F) | 95.7 (A) |
| Entrypoints | 114 | 13 | 4 | 6 |
| Accounted for | 95.5% | 34.7% | 37.1% | 98.7% |
| Unresolved | 4.5% | 64.3% | 62.8% | 1.3% |
| Reported dead | 1 | 38 | 1 | 0 |
| Verified unreferenced | 1 | 38 | 1 | n/a |
| Mentioned but not traced | 2 | 8 | 2 | 0 |
| Tests / reported unable to fail | 4 243 / 1 | 59 / 0 | 75 / 0 | 372 / 0 |

The previous build reported 241 dead on Reth, 482 on lc0 and 114 on Stockfish,
and about 1 in 10 of a random sample was actually dead.

The dead symbols:

- Reth: `TxPool::hashes_iter` (`crates/transaction-pool/src/pool/txpool.rs`,
  `pub(crate)`). It carries `#[expect(dead_code)]`, so rustc agrees.
- Stockfish: `m512_hadd` (`src/nnue/simd.h`, `[[maybe_unused]] static`).
- lc0: 30 of 38 appear nowhere outside their definition: 16 `EscCodes`
  accessors in `src/utils/esc_codes.h`, 8 Python methods in `scripts/pybind/`
  and `scripts/compile_proto.py`, `FP32toFP8E4M3FN`, `FP8E4M3FNtoFP32`,
  `GetItemStructSize`, `GetMaxCollisionEvents`, `as_proto` and `GetSycl_Queue`.
  The other 8 occur only as their own prototype or as same-named definitions in
  other classes: `UglyFunctionToSilenceNvccWarning` (2), `DumpFp32`,
  `value_as_string`, `GetNumOutputs`, `GetSize` and `GetCapacity` in `cache.h`,
  and `GetCapacity` in the orphaned `cache-old.h`. No `##` macro or `getattr`
  could build any of these names.

Unreferenced does not mean the code should be deleted. Several of lc0's are
parts of complete APIs, such as the colour palette and cache accessors.

The one test reported unable to fail, Reth's `issue_12054`
(`crates/exex/exex/src/context.rs`), is a `const fn` whose body only declares a
type and an impl. It is a compile-time bounds check. lc0's 59 test functions
match its 59 googletest `TEST`/`TEST_F` cases.

### Recall

Three uniquely named unused functions were planted in a copy of each
repository:

| Target | Planted | Reported dead |
|---|---|---|
| lc0 | `static` function in `utils/string.cc`; free function with a prototype in `utils/string.h`; `ChessBoard` member defined out of class in `board.cc` | 3/3 |
| Reth | private `fn` in `crates/tasks/src/shutdown.rs`; `pub(crate) fn` in `crates/net/discv4/src/config.rs`; `pub(super) fn` in a nested module of `crates/trie/common/src/prefix_set.rs` | 3/3 |

Only a bare `pub` counts as public API. `pub(crate)`, `pub(super)`, `pub(self)`
and `pub(in path)` items are crate-private, so an unused one is dead. This
change also raised Reth's unresolved share from 3.0% to 4.5%.

### Limitations

- lc0 and Stockfish are `INCONCLUSIVE`. Only 35-37% of each resolves, below the
  50% needed for a verdict.
- A bare `pub` item inside a private Rust module still counts as public API,
  because reachability from the crate root is not traced.
- A dead symbol whose name appears anywhere else (a common word, a comment) is
  reported as "mentioned but not traced", not as dead. The 12 such entries above
  were not classified individually. lc0's `Reverse`, `Normal` and `Black` sit
  beside 16 dead `EscCodes` siblings and are probably unused too.
- "Public surface" includes exported functions, not just entrypoints. Stockfish
  reports "0 of 32 exported functions tested" with 4 entrypoints.
- Graph and coverage are not cached (840 ms of a 1.5 s warm Reth run).
- Only one Windows machine was measured. Nothing larger than Reth has been
  tested, and the synthetic curve has not been re-run on the parallel build.

## Cache correctness

`tests/determinism.rs` checks that a cached audit, a fresh audit and an audit
after deleting the cache produce byte-identical JSON; that editing a file
invalidates its entry and reverting the edit restores the original result; and
that a corrupted cache is treated as a miss.

## Reproducing

See [`bench/README.md`](bench/README.md). For stage timings on any repository:

```sh
CODIE_TIMING=1 codereader audit <repo> --json > /dev/null
CODIE_THREADS=4 codereader audit <repo> --json > /dev/null
```
