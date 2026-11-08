# Codie

Codie is a static auditing tool for source repositories. It finds every
entrypoint (HTTP routes, CLI commands, jobs, `main`), builds a call graph with
tree-sitter, and reports code that nothing reaches, entrypoints that no test
exercises, and tests that assert nothing. The audit runs offline and needs no
model or API key. The binary is called `codereader`.

```
$ codereader audit .
117 files - 1117 symbols - 6 entrypoints (5 external)

CLEAN  score 95.7 (A)   confidence 99%

Coverage
  98.7% of 1123 meaningful nodes accounted for (15 unaccounted)

Not proven either way
  - 15 symbol(s) are referenced but could not be traced to any root. Static analysis cannot prove these either way.
  - 48 symbol(s) are reachable only through polymorphic dispatch. They are counted as covered, but which implementation runs was inferred, not proven.

Test coverage of the public surface
  258 of 285 entrypoints and exported functions are exercised by a test (91%)
  369 test function(s); 0 assert nothing
```

## Installation

Prebuilt binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the
[releases page](https://github.com/moezii/Coodie/releases).

To build from source you need Rust 1.85 or newer and a C compiler (the
tree-sitter grammars are written in C):

```sh
cargo build --release          # target/release/codereader
cargo install --path .         # or install into ~/.cargo/bin
cargo test
```

On Windows with MSYS2, put the toolchain on `PATH` first:
`export PATH="/c/msys64/ucrt64/bin:$PATH"`.

## Usage

```sh
codereader audit ./repo                # terminal summary
codereader audit ./repo --report       # Markdown report
codereader audit ./repo --json         # machine-readable, byte-identical across runs
codereader audit ./repo --limit 0      # list every finding (default 20)
codereader audit ./repo --describe     # add plain-English descriptions (needs a model)
```

The audit prints one of three verdicts:

| Verdict | Meaning |
|---|---|
| `CLEAN` | Nothing found wrong. |
| `FINDINGS` | Dead code, untested entrypoints or tests that cannot fail. |
| `INCONCLUSIVE` | Too little of the code could be resolved to judge it. |

Confidence is the share of the call graph that resolved. Code Codie could not
trace is listed under "Not proven either way" and is not counted against the
repository.

### CI

```sh
codereader audit . --fail-under 60          # exit non-zero below this score
codereader audit . --fail-on-untested       # exit non-zero if an entrypoint has no test
codereader audit . --json > base.json
codereader audit . --baseline base.json     # fail only if the score dropped or new untested entrypoints appeared
```

`.github/workflows/codie-audit.yml` runs the baseline check against the target
branch, so an existing backlog does not fail the build. `hooks/pre-commit` is a
pre-commit hook; copy it to `.git/hooks/`.

### Other commands

```sh
codereader find "where verify_token is defined" -p ./repo
codereader find "show me every external POST request" -p ./repo
codereader find "the payment charge" -p ./repo --blame
codereader explain 1 -p ./repo         # explain result 1 of the last find (uses a model)
codereader index ./repo                # parse and build the symbol table
codereader watch ./repo                # reparse files as they change
codereader cache --stats -p ./repo
codereader cache --clear -p ./repo
```

`find` answers structural queries (definitions, call sites, HTTP calls)
directly from the syntax tree. Conceptual queries ("the function that issues a
session token") are ranked by a model. See [USAGE.md](USAGE.md) for the query
forms, JSON fields and troubleshooting.

Output is styled on a terminal and plain when piped; `--color auto|always|never`
overrides it.

## How it works

1. Entrypoints are matched from framework conventions in
   `src/audit/conventions.rs` and per-language queries in `src/audit/queries/`.
   A registration is recognised by the shape of the call: it passes a function
   and discards the result. `await api.get("/users")` in a frontend is a client
   call, not a route.
2. Files, symbols and entrypoints become a graph of `Contains`, `Calls` and
   `HandledBy` edges. A call that could resolve to more than one target gets no
   edge.
3. Reachability is computed from four root sets: external entrypoints, startup
   paths, public API and tests. A library is judged by its exports and a CLI by
   its `main`.
4. Before a symbol is reported dead, its name is searched for in every text file
   in the repository. If it appears anywhere else, it is listed as "mentioned but
   not traced" instead.
5. Tests are found by each language's own marker (`#[cfg(test)]`, `@Test`,
   `[Fact]`, `func TestX(*testing.T)`, pytest `test_*`), not by directory name.

The score is out of 100: coverage 40, clarity (how much resolved) 30, structure
15, tests 15. Parse results are cached in `.codereader/cache.sqlite3`, keyed by
file content hash. Design details are in [ARCHITECTURE.md](ARCHITECTURE.md).

Supported languages: Shell, C, C++, C#, CSS, Go, HTML, Java, JavaScript, JSON,
Kotlin, OCaml, PHP, Python, R, Ruby, Rust, Scala, Swift, TOML, TSX, TypeScript.
Entrypoint detection covers Express, Fastify, Koa, Hono, FastAPI, Flask,
Starlette, Django, Celery, Click, Typer, net/http, Gin, Spring, actix, axum,
rocket, ASP.NET, Sinatra, Rails, Laravel and Symfony, plus `main` in any
language. About 40 other languages are detected but not parsed; they are
reported as `Baseline` support with zero symbols.

## Accuracy and performance

Every symbol reported dead on Reth, lc0 and Stockfish was checked by hand: 40 of
40 were unreferenced. Planted unused functions were all found (3 of 3 on Reth
and on lc0). Most of the two C++ engines is still unresolved, so both get
`INCONCLUSIVE`. A warm audit of Reth (23,744 symbols) takes 1.5 s; repositories
up to about 4,000 symbols take under 0.2 s warm. Method, results and known
limitations are in [BENCHMARKS.md](BENCHMARKS.md). Planned work is in
[ROADMAP.md](ROADMAP.md).

## Model configuration

`audit` never uses a model. `audit --describe`, conceptual `find` and `explain`
do. Configure one with `CODIE_LLM=provider,model,key,base_url` (trailing fields
optional), or pass `--llm` with the same value:

```sh
export CODIE_LLM="anthropic,claude-haiku-4-5-20251001,sk-ant-..."
export CODIE_LLM="openai,gpt-4o-mini,sk-proj-..."
export CODIE_LLM="deepseek,deepseek-chat,sk-..."
export CODIE_LLM="ollama,qwen2.5-coder"     # local, no key
```

An existing `OPENAI_API_KEY` or `DEEPSEEK_API_KEY` also works. Codie prints
which provider it picked.

What is sent:

| Command | Sent to the model |
|---|---|
| `audit`, structural `find` | nothing |
| `audit --describe`, conceptual `find` | function and route names, never file contents |
| `explain <n>` | the one snippet you asked about |

Approved models, service policies (`CODIE_LLM_POLICY`) and gateway
settings are described in [USAGE.md](USAGE.md#models).

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
