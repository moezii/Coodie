# Architecture

Notes for contributors on how the audit pipeline is structured and what to
keep in mind when changing it.

## Pipeline

```
walk            index/walker.rs, honours .gitignore
facts           audit/mod.rs::extract_file_facts: symbols, entrypoints, calls, docstrings (one parse per file)
graph.rs        nodes and edges, layered call resolution
reachability.rs one BFS per root set, adjacency built once
mentions.rs     name check before anything is called dead
coverage.rs     classify every meaningful node
testing.rs      untested surface, assertion-free tests
score.rs        score and verdict
report.rs       Markdown report
describe.rs     optional prose and claim checking (model)
```

`find` and `explain` live in `src/query/` and `src/llm/` and share the index and
cache.

### Call resolution

`graph.rs::resolve_call` tries, in order: self scope, `Type::method`,
`module.method`, unique in file, globally unique. If no step gives exactly one
target, no edge is created. A guessed edge could make dead code look reachable.

Interface and trait method declarations count as evidence that their
implementations are reachable.

### Root sets

External entrypoints, startup paths, public API and tests. A service is judged
by its routes, a library by its exports and a CLI by `main`.

### Mention gate

For each symbol reachability would mark dead, `mentions.rs` searches for its
name as a whole word in every file the walker yields, after ignore rules and
vendored exclusion. Prose docs, binaries and files over 16 MiB are skipped. A
hit outside the symbol's own definition makes it `MentionedNotTraced`
(unresolved, listed with the first mention). Same-named definitions block each
other; only declarations reported by an extractor
(`FileFacts::declaration_sites`, for C/C++ prototypes) are excused. The gate is
language-independent, so a construct no extractor handles cannot produce a
false "dead". It is not cached and runs in parallel on the candidate names only
(about 40 ms on Reth).

### Registration vs. client calls

Route registration is recognised by call shape: a registration passes a
function and discards the result, a client request passes options and uses the
result. `myBackend.get("/users", handler)` registers;
`await api.get("/auth/profile")` does not. The receiver allowlist (`app`,
`router`, ...) is only a tiebreaker.

### Parse cache

Per-file facts are stored in SQLite under `.codereader/`, keyed by content hash.
Graph, reachability and coverage are rebuilt on every run.

## Design rules

1. Code that cannot be traced is unresolved, not dead. This covers dynamic
   dispatch, dependency injection, macro bodies, framework callbacks,
   decorators, functions passed as values, trait impls, `__all__` and type
   annotations.
2. Findings and "Not proven either way" are reported in separate sections. The
   second describes the limits of the analysis, not the audited code.
3. An ambiguous call produces no edge.
4. Prefer language rules to naming habits. `#[cfg(test)]` is a rule; a module
   named `tests` is a habit.
5. The model in `--describe` only describes facts from the graph. Every node it
   cites is verified.

Known cases where rule 1 does not yet hold are listed under Limitations in
[BENCHMARKS.md](BENCHMARKS.md#limitations).

Signals that still depend on naming:

| Signal | Kind |
|---|---|
| Rust `#[cfg(test)]`, pytest `test_*`, Go `_test.go` | rule |
| JS/TS `.test.` / `.spec.` | convention |
| `dist/`, `build/`, `target/` | convention |
| Receiver allowlist (`app`, `router`, ...) | tiebreaker |

## Changing extraction

Bump `FACTS_VERSION` in `src/audit/mod.rs` whenever extraction changes. Adding a
grammar or fixing a query does not change any file's bytes, so without the bump
the cache keeps returning old results (a new grammar will appear to yield zero
symbols).

A new framework is a set of rows in `src/audit/conventions.rs`. A new language
needs a `*_registrations.scm` file in `src/audit/queries/`. Registration queries
are compiled by the test suite, so a malformed one fails a test.

## Tests

| File | Contents |
|---|---|
| `src/**` unit tests | each stage in isolation |
| `tests/audit_integration.rs` | whole pipeline over fixture repositories |
| `tests/language_coverage.rs` | every grammar parses and yields symbols |
| `tests/edge_cases.rs` | entrypoint shapes, lookalikes, serving vs. calling |
| `tests/never_falsely_accuse.rs` | design rule 1 in nine languages, plus controls |
| `tests/mention_gate.rs` | the mention gate across Makefile, YAML, Dockerfile, `dlsym` strings and cross-language strings; recall controls |
| `tests/test_detection.rs` | each language's test marker under misleading names |
| `tests/hostile_repositories.rs` | unparseable, config-only and empty repositories, CRLF, BOM |
| `tests/determinism.rs` | repeated runs agree, JSON is byte-identical, cache invalidation |
| `tests/report_output.rs` | Markdown report stability and wording |
| `tests/describe_live.rs` | model path; skipped without a configured provider |

Two cases in `never_falsely_accuse.rs` are controls: an unreachable function
must still be reported, and declaring an interface must not excuse an unrelated
name. Without them the file would pass if the audit reported nothing. Keep
them.
