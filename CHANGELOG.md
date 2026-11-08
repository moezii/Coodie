# Changelog

All notable changes to this project are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project uses [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-11-08

First release.

### Added

- `codereader audit`: builds a call graph with tree-sitter and reports
  entrypoints, unreachable code, entrypoints with no test and tests that assert
  nothing. Runs offline without a model.
- Verdicts `CLEAN`, `FINDINGS` and `INCONCLUSIVE` (too little resolved to judge).
- A symbol is reported dead only if it is unreachable and its name appears
  nowhere else in the repository. Otherwise it is listed as unresolved.
- C and C++ support for headers, out-of-class methods, constructors,
  operators, virtual overrides and macros.
- Rust support for trait dispatch, references in attribute strings and
  restricted visibility (`pub(crate)` and similar).
- Python `__main__` blocks as entrypoints.
- Detection and exclusion of vendored code (`vendor/`, `third_party/`, git
  submodules, nested foreign projects).
- Test detection for Rust test attributes, googletest, Catch2, Boost.Test,
  pytest and JavaScript test runners.
- Terminal, Markdown (`--report`) and JSON (`--json`) output.
- CI options `--fail-under`, `--fail-on-untested` and `--baseline`, with a
  GitHub Actions workflow and a pre-commit hook.
- Parallel parsing and a SQLite parse cache keyed by SHA-256 content hash.
  Reth (1,705 files, 23,744 symbols) audits in 4.1 s cold and 1.5 s warm.
- `--describe`: plain-English feature descriptions and a check of documented
  claims against the code, using Anthropic, OpenAI, DeepSeek or a local Ollama
  model.
- `find`, `explain`, `index`, `watch` and `cache` commands.

[0.1.0]: https://github.com/moezii/Coodie/releases/tag/v0.1.0
