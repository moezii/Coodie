# Usage reference

Details that do not fit in the [README](README.md). All commands take `-p PATH`
(or a positional path for `audit`, `index` and `watch`) and default to the
current directory.

## audit

| Flag | Effect |
|---|---|
| `--json` | Full machine-readable report |
| `--report` | Markdown report |
| `--describe` | Add plain-English feature descriptions and check documented claims against the code (needs a model) |
| `--llm SPEC` | Override `CODIE_LLM` for this run |
| `--limit N` | Number of unaccounted findings to list, default 20, 0 for all |
| `--fail-under SCORE` | Exit non-zero if the score is below `SCORE` |
| `--fail-on-untested` | Exit non-zero if any externally reachable entrypoint has no test |
| `--baseline FILE` | Exit non-zero if the score dropped or new untested entrypoints appeared since the `--json` report in `FILE` |

The JSON output is byte-identical across runs on unchanged code. Useful fields:
`score.score`, `verdict.status`, `coverage.coverage_percent`,
`coverage.unaccounted[]`, `tests`, `entrypoints[]`.

```sh
codereader audit . --json | jq '.score.score, .verdict.status'
```

`--describe` reads claims from docstrings, `docs/`, OpenAPI specs, package
manifests, changelogs and the README, and compares them with what each
documented function calls. Every node the model cites is checked against the
graph.

## find

Structural queries run locally:

| Query | Result |
|---|---|
| `where verify_token is defined`, `function named parseConfig` | the definition |
| `the function containing fetch` | the function enclosing a call |
| `call to charge`, `calls to function charge` | call sites |
| `the POST request`, `fetch` | HTTP call sites |
| `requests.post` | call sites, with import aliases resolved (`import requests as r`) |
| `verify_token` | exact function match |

Other queries are conceptual. The model ranks the candidates found in the
syntax tree; it does not see the rest of the repository. One clear match is
shown directly, several are shown as a numbered list. A conceptual query with
no model configured fails with a message saying what to set.

A query containing "all", "every" or "everywhere" returns every match with its
score.

`--blame` shows the last author, date and commit of each match. `--flag-ai`
shows a heuristic guess that a match may be AI-generated; it is off by default
and is not reliable.

## explain

`codereader explain N` explains result `N` of the previous `find`. It describes
what the code does and why it matched. It does not suggest changes; the only
problems it reports are objective ones such as syntax errors or unresolved
imports. Responses are streamed and cached.

## Cache

`.codereader/cache.sqlite3` (gitignored) holds parse results keyed by file
content hash, query results keyed by query and repository state, and model
responses keyed by prompt and model. Switching branches reuses entries for
content already seen. Committed changes are found by diffing the recorded
`HEAD` against the current one, uncommitted ones by `git status` and the
watcher.

Only parsing is cached. The graph and coverage stages run every time.

## Models

See the README for `CODIE_LLM` and what each command sends.

Model ids are listed in `src/llm/registry.rs` in two tiers, `fast` (search
ranking) and `strong` (descriptions). Unlisted models are rejected. Override a
tier with `{PROVIDER}_FAST_MODEL` or `{PROVIDER}_STRONG_MODEL`.

`CODIE_LLM_POLICY` controls whose configuration applies when Codie runs as a
service:

| Policy | Model chosen by | Endpoint chosen by | Reads the audited repo's `.env` |
|---|---|---|---|
| `local` (default) | user | user | yes |
| `managed` | operator | operator | no |
| `enterprise` | operator | customer | no |

For gateways and Azure OpenAI, set `CODIE_LLM_AUTH` (`bearer`, the default,
`api-key`, `x-api-key` or `header:<Name>`) and `CODIE_LLM_QUERY`:

```sh
export CODIE_LLM="openai,gpt-4o-mini,<key>,https://acme.openai.azure.com/openai/deployments/gpt4o"
export CODIE_LLM_AUTH=api-key
export CODIE_LLM_QUERY="api-version=2024-06-01"
```

AWS Bedrock is not supported directly because it requires SigV4 request
signing. Use an OpenAI-compatible gateway such as LiteLLM or the Bedrock Access
Gateway.

## Environment

| Variable | Effect |
|---|---|
| `CODIE_LLM` | Model, as `provider,model,key,base_url` |
| `CODIE_LLM_POLICY` | `local`, `managed` or `enterprise` |
| `CODIE_LLM_AUTH` | `bearer`, `api-key`, `x-api-key` or `header:<Name>` |
| `CODIE_LLM_QUERY` | Query string appended to model requests |
| `CODIE_THREADS` | Parser worker count (default: one per logical CPU) |
| `CODIE_TIMING` | Print per-stage timings to stderr |

## Troubleshooting

| Symptom | Fix |
|---|---|
| `Failed to find tool. Is gcc.exe installed?` or a `dlltool.exe` error while building | Install a C toolchain. On MSYS2, `export PATH="/c/msys64/ucrt64/bin:$PATH"`. |
| `no LLM configured; set CODIE_LLM ...` | Set `CODIE_LLM` or phrase the query structurally. |
| `provider transport error ... Connection refused` | The endpoint is not running or `base_url` is wrong. |
| Verdict is `INCONCLUSIVE` | Usually a framework Codie does not model, or a language without a grammar. |
| A language shows 0 symbols | No bundled grammar; it is reported as `Baseline`. |
| Results look stale | `codereader cache --clear -p .` |
| A file is missing from the results | It is gitignored, binary, or an unsupported type. |
