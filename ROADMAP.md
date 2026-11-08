# Roadmap

Planned work, roughly in priority order. Current numbers are in
[BENCHMARKS.md](BENCHMARKS.md).

## Accuracy

- Resolve more C++. lc0 and Stockfish are only 35-37% resolved and get
  `INCONCLUSIVE`. Most of the gap is member calls and dispatch that the graph
  does not trace to a root.
- Trace Rust module privacy from the crate root, so a bare `pub` item in a
  private module can be reported dead.
- Sandboxed mutation testing, to find tests that assert something but still
  cannot catch a change.

## Performance

- Profile graph and coverage, which still grow faster than linearly (11.8x the
  time for 6.5x the symbols from lc0 to Reth). Measure on the synthetic chain at
  8k, 16k, 32k and 64k symbols and on Reth.
- Make single-threaded extraction cheaper. One thread takes 20.0 s on cold Reth
  against 15.9 s for the old serial parser, which matters on 2-core CI runners.
- Share the parse cache across checkouts with a user-level cache directory.
  Entries are keyed by content hash, so they are valid in any clone.
- Cache or incrementally update the graph, after the superlinear step is fixed.
  `tests/determinism.rs` should then also check that incremental results equal
  a full run over random edit sequences.
- Intern identifiers and use bitsets in reachability, if profiling shows the
  graph is still the bottleneck on large repositories.

GPU acceleration is not planned. Parsing and sparse graph traversal are
branch-heavy and latency-bound. The one plausible use is local embeddings for
conceptual `find`, and that would be optional.

## Memory and resource safety

Checks limited to what can be shown on a single path inside one function:

1. Rust `unsafe` blocks without a `// SAFETY:` comment.
2. Acquire without release on some path: `malloc`/`free` in C, files and
   connections in Go, Java and C#.
3. Double free and straight-line use after free in C and C++.

Anything that needs alias or interprocedural analysis (use after free across
functions, function pointers, virtual dispatch, concurrency) would be listed
under "Not proven either way", not reported as a finding.

## Benchmarks

- Linux and macOS runs.
- A repository larger than Reth.
- Re-run the synthetic scaling curve on the parallel build.
