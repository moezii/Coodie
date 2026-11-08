//! The audit engine.
//!
//! Codie's `find` answers "where is the code that does X". The audit answers a
//! harder question: "what does this repository actually do, and what part of it
//! can nobody account for?"
//!
//! The pipeline is deterministic end to end — no model is involved:
//!
//! ```text
//! index (symbols) ─┐
//!                  ├─> graph (nodes + edges) ─> reachability ─> coverage ─> score
//! entrypoints ─────┘
//! ```
//!
//! Unexplained code is the finding that matters. A repository where every symbol
//! is reachable from a known entrypoint is one you can reason about; a repository
//! with forty unreachable exported functions is hiding something.

mod cfamily;
pub mod claims;
pub mod conventions;
pub mod coverage;
pub mod describe;
pub mod entrypoints;
pub mod graph;
pub mod jsx;
mod parallel;
pub mod reachability;
pub mod report;
pub mod rust_facts;
pub mod score;
pub mod testing;
pub mod mentions;
pub mod vendor;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cache::{CacheError, CacheStore};
use crate::index::{discover_files, IndexError, LineRange, SourceLanguage, Symbol};

pub use coverage::{Classification, CoverageFinding, CoverageReport};
pub use graph::{Edge, EdgeType, Graph, GraphNode, NodeKind};
pub use reachability::{Reachability, ReachabilityIndex};
pub use score::{Score, Verdict, VerdictStatus};

#[derive(Debug, Error)]
pub enum AuditError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error("failed to read source file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to serialize audit report: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error(transparent)]
    Model(Box<crate::llm::LlmError>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EntryPointKind {
    Http,
    WebSocket,
    Cli,
    Event,
    Scheduled,
    GraphQl,
    Application,
    Startup,
}

impl EntryPointKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::WebSocket => "websocket",
            Self::Cli => "cli",
            Self::Event => "event",
            Self::Scheduled => "scheduled",
            Self::GraphQl => "graphql",
            Self::Application => "application",
            Self::Startup => "startup",
        }
    }
}

/// Whether an entrypoint is reachable from outside the process, or only runs
/// as part of starting it up. Startup code is infrastructure, not a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exposure {
    External,
    Startup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPoint {
    pub kind: EntryPointKind,
    pub name: String,
    pub exposure: Exposure,
    pub http_method: Option<String>,
    pub route: Option<String>,
    pub handler_name: Option<String>,
    pub file: PathBuf,
    pub line_range: LineRange,
    pub framework: String,
}

impl EntryPoint {
    pub fn id(&self, relative_file: &str) -> String {
        format!(
            "entrypoint:{}:{}:{}:{}",
            self.kind.label(),
            relative_file,
            self.line_range.start,
            self.name
        )
    }
}

/// How much the engine can prove for a given language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupportLevel {
    /// Files, symbols, and call sites.
    Baseline,
    /// Baseline plus framework routes, jobs, commands, and startup paths.
    Entrypoints,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageSupport {
    pub files: usize,
    pub symbols: usize,
    pub support: SupportLevel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditReport {
    pub root: PathBuf,
    pub files: usize,
    pub symbols: usize,
    pub entrypoints: Vec<EntryPoint>,
    pub languages: BTreeMap<String, LanguageSupport>,
    pub coverage: CoverageReport,
    pub tests: testing::TestAnalysis,
    pub score: Score,
    pub verdict: Verdict,
    /// Plain-English descriptions. Present only when `--describe` was used;
    /// everything else in this report is deterministic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrative: Option<describe::Narrative>,
    /// Everything the repository asserts about itself, and where it says it.
    #[serde(default)]
    pub claims: claims::ClaimSet,
    /// Directories holding vendored third-party code, excluded from the audit.
    /// See [`vendor`] for the rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendored: Vec<String>,
    /// The graph, kept so the description pass can run without re-analysing.
    /// Reachability is cheap to rebuild from it, so it is not stored.
    #[serde(skip)]
    pub graph: Graph,
}

impl AuditReport {
    pub fn to_json(&self) -> Result<String, AuditError> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

/// One file read and hashed, waiting to be decoded from the cache or parsed.
struct LoadedFile {
    source: Vec<u8>,
    relative: String,
    content_hash: String,
    line_count: usize,
}

/// Run a full deterministic audit of a repository.
///
/// Each file is parsed exactly once; symbols, entrypoints, and call sites all
/// come off the same tree.
pub fn audit_repository(path: impl AsRef<Path>) -> Result<AuditReport, AuditError> {
    let requested = path.as_ref();
    let root = requested
        .canonicalize()
        .map_err(|source| IndexError::CanonicalizePath {
            path: requested.to_path_buf(),
            source,
        })?;

    let mut timer = StageTimer::start();
    let walked = discover_files(&root)?;
    let source_files = walked.source_files;
    // Someone else's project bundled inside this one is accounted for by its
    // upstream. Auditing it reports the upstream's API as this project's dead
    // code, so it is left out — and named in the report, never silently.
    let relative_files: Vec<String> = source_files
        .iter()
        .map(|file| relative_path(&root, &file.path))
        .collect();
    let vendored = vendor::vendored_directories(&root, &relative_files);
    let source_files: Vec<_> = source_files
        .into_iter()
        .zip(relative_files)
        .filter(|(_, relative)| !vendor::is_inside(relative, &vendored))
        .map(|(file, _)| file)
        .collect();
    // The final dead-code check reads every text file that survived the same
    // exclusions, in any language or none. Prose is left out: see `mentions`.
    let mut mention_files: Vec<mentions::TextFile> = source_files
        .iter()
        .map(|file| &file.path)
        .chain(&walked.other_files)
        .map(|path| mentions::TextFile {
            relative: relative_path(&root, path),
            path: path.clone(),
        })
        .filter(|file| {
            !vendor::is_inside(&file.relative, &vendored) && !mentions::is_prose(&file.relative)
        })
        .collect();
    mention_files.sort_by(|left, right| left.relative.cmp(&right.relative));
    timer.mark("walk");
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut all_entrypoints: Vec<EntryPoint> = Vec::new();
    let mut calls: Vec<graph::CallSite> = Vec::new();
    let mut mentioned_names: Vec<String> = Vec::new();
    // Per file, where a function's name is declared rather than used.
    let mut declaration_sites: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    let mut asserting_symbols: Vec<String> = Vec::new();
    let mut declared_tests: Vec<testing::DeclaredTest> = Vec::new();
    let mut dispatched: Vec<graph::DispatchedMethod> = Vec::new();
    let mut test_functions: Vec<String> = Vec::new();
    let mut all_claims: Vec<claims::Claim> = Vec::new();
    let mut languages: BTreeMap<String, LanguageSupport> = BTreeMap::new();
    let mut indexed_files: Vec<IndexedFile> = Vec::new();

    // Bump when extraction changes what a parse produces. Adding a grammar or
    // fixing a query does not alter any file's bytes, so without this the cache
    // keeps serving results from before the change — which is how newly
    // supported languages came back with zero symbols.
    const FACTS_VERSION: &str = "9";

    // Parsing is by far the most expensive stage — measured at roughly 90% of a
    // release-build audit — and almost none of it is new work on a second run.
    // A cache failure must never fail an audit, so every cache operation here
    // degrades to "parse it again" rather than propagating an error.
    let facts_cache = CacheStore::open_for_root(&root).ok();

    // Files are read, hashed and parsed on every core. Only the SQLite
    // lookups and the single batched write stay on this thread, because the
    // connection is not shareable. Results are folded back in walk order, so
    // the report is the same at any thread count.
    let threads = parallel::thread_count();
    let loaded = parallel::map(&source_files, threads, |source_file| {
        let source = fs::read(&source_file.path).map_err(|source| AuditError::ReadFile {
            path: source_file.path.clone(),
            source,
        })?;
        let relative = relative_path(&root, &source_file.path);

        // Keyed by content, not path: renaming a file, switching branches, or
        // reverting an edit all return to a state already parsed.
        let content_hash = crate::cache::hash_parts(&[
            source.as_slice(),
            relative.as_bytes(),
            source_file.language.cache_label().as_bytes(),
            FACTS_VERSION.as_bytes(),
        ]);
        Ok(LoadedFile {
            line_count: line_count(&source),
            source,
            relative,
            content_hash,
        })
    });
    // The first unreadable file in walk order fails the audit, as before.
    let loaded: Vec<LoadedFile> = loaded.into_iter().collect::<Result<_, AuditError>>()?;

    let cached_payloads: Vec<Option<String>> = source_files
        .iter()
        .zip(&loaded)
        .map(|(source_file, file)| {
            facts_cache.as_ref().and_then(|store| {
                store
                    .get_file_facts_json(&file.content_hash, &source_file.language)
                    .ok()
                    .flatten()
            })
        })
        .collect();

    let caching = facts_cache.is_some();
    let jobs: Vec<(&crate::index::SourceFile, &LoadedFile, Option<&str>)> = source_files
        .iter()
        .zip(&loaded)
        .zip(&cached_payloads)
        .map(|((source_file, file), payload)| (source_file, file, payload.as_deref()))
        .collect();
    // A payload this version cannot read is a miss, never an error.
    let extracted: Vec<(graph::FileFacts, Option<String>)> =
        parallel::map(&jobs, threads, |(source_file, file, payload)| {
            let cached = payload.and_then(|json| serde_json::from_str(json).ok());
            match cached {
                Some(facts) => (facts, None),
                None => {
                    let facts =
                        extract_file_facts(&source_file.language, &file.relative, &file.source);
                    let payload = if caching {
                        serde_json::to_string(&facts).ok()
                    } else {
                        None
                    };
                    (facts, payload)
                }
            }
        });
    drop(jobs);
    drop(cached_payloads);

    if let Some(store) = facts_cache.as_ref() {
        let fresh: Vec<(&str, &SourceLanguage, &str)> = source_files
            .iter()
            .zip(&loaded)
            .zip(&extracted)
            .filter_map(|((source_file, file), (_, payload))| {
                payload.as_deref().map(|payload| {
                    (file.content_hash.as_str(), &source_file.language, payload)
                })
            })
            .collect();
        if !fresh.is_empty() {
            let _ = store.put_file_facts_json_batch(fresh);
        }
    }

    for ((source_file, file), (facts, _)) in source_files.iter().zip(loaded).zip(extracted) {
        let support = if entrypoints::has_entrypoint_support(&source_file.language) {
            SupportLevel::Entrypoints
        } else {
            SupportLevel::Baseline
        };
        let entry = languages
            .entry(source_file.language.id().to_owned())
            .or_insert(LanguageSupport {
                files: 0,
                symbols: 0,
                support,
            });
        entry.files += 1;
        entry.symbols += facts.symbols.len();

        indexed_files.push(IndexedFile {
            relative: file.relative,
            language: source_file.language.clone(),
            line_count: file.line_count,
            symbol_count: facts.symbols.len(),
        });

        symbols.extend(facts.symbols);
        all_entrypoints.extend(facts.entrypoints);
        calls.extend(facts.calls);
        mentioned_names.extend(facts.mentioned_names);
        if !facts.declaration_sites.is_empty() {
            let relative = indexed_files.last().map(|indexed| indexed.relative.clone());
            declaration_sites.insert(relative.unwrap_or_default(), facts.declaration_sites);
        }
        asserting_symbols.extend(facts.asserting_symbols);
        declared_tests.extend(facts.declared_tests);
        dispatched.extend(facts.dispatched);
        test_functions.extend(facts.test_functions);
        all_claims.extend(facts.docstrings);
    }

    // Prose claims live in files the parser never opens — READMEs, docs/,
    // OpenAPI specs, changelogs, manifests.
    let scanned_paths: Vec<String> = source_files
        .iter()
        .map(|file| relative_path(&root, &file.path))
        .collect();
    all_claims.extend(claims::collect_file_claims(&root, &scanned_paths).claims);
    let claim_set = claims::ClaimSet { claims: all_claims };

    timer.mark("parse + extract");
    mentioned_names.sort_unstable();
    mentioned_names.dedup();
    let built = graph::build_with_mentions(
        &indexed_files,
        &symbols,
        &all_entrypoints,
        &calls,
        &mentioned_names,
        &dispatched,
    );
    timer.mark("graph");
    let mut built = built;
    let mut reachability = ReachabilityIndex::build(&built);
    timer.mark("reachability");
    // Last word before anything is called dead: is its name written anywhere
    // else? If so it is unresolved, whatever language or file says it.
    let dead: std::collections::HashSet<&str> = reachability.dead_ids().collect();
    let candidates: Vec<mentions::Candidate> = built
        .nodes
        .iter()
        .filter(|node| dead.contains(node.id.as_str()))
        .map(|node| mentions::Candidate {
            node_id: node.id.clone(),
            name: node.name.clone(),
            file: node.file.clone(),
            start_line: node.line_range.start,
            end_line: node.line_range.end.max(node.line_range.start),
        })
        .collect();
    drop(dead);
    built.untraced_mentions = mentions::find_untraced(&mention_files, &candidates, &declaration_sites, threads);
    reachability.apply_untraced_mentions(&built);
    timer.mark("mention gate");
    let coverage = coverage::review(&built, &reachability);
    asserting_symbols.sort_unstable();
    asserting_symbols.dedup();
    let evidence =
        testing::collect_evidence(&symbols, &calls, &asserting_symbols, &declared_tests);
    let tests = testing::analyse_with(&built, &reachability, &evidence, &test_functions);
    let score = score::score(&coverage);
    let verdict = score::verdict(&coverage, &score);
    timer.mark("coverage + tests + score");
    timer.total();

    all_entrypoints.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line_range.start.cmp(&right.line_range.start))
    });

    Ok(AuditReport {
        root,
        files: indexed_files.len(),
        symbols: symbols.len(),
        entrypoints: all_entrypoints,
        languages,
        coverage,
        tests,
        score,
        verdict,
        narrative: None,
        claims: claim_set,
        vendored,
        graph: built,
    })
}

/// Add plain-English descriptions and a documentation check to a finished audit.
///
/// Kept separate from [`audit_repository`] so the deterministic result never
/// depends on a model being configured, reachable, or in a good mood.
pub fn describe_report(
    report: &mut AuditReport,
    provider: &mut dyn crate::llm::LlmProvider,
    cache: Option<&CacheStore>,
) -> Result<(), AuditError> {
    let reachability = ReachabilityIndex::build(&report.graph);
    let graph = report.graph.clone();
    let narrative = describe::narrate(report, &graph, &reachability, provider, cache)?;
    report.narrative = Some(narrative);
    Ok(())
}

/// Parse one file and pull every fact the graph needs off a single tree.
fn extract_file_facts(
    language: &SourceLanguage,
    relative: &str,
    source: &[u8],
) -> graph::FileFacts {
    let Some(grammar) = crate::index::grammar_for(language) else {
        // CUDA, shaders, Objective-C++ and include fragments have no grammar
        // here but call into C and C++ code that does. Every word in them is a
        // possible reference; none of them defines a symbol.
        if cfamily::is_lexical_reference_language(language.id()) {
            let mut mentioned_names = Vec::new();
            cfamily::tokenize_identifiers(
                &crate::index::strip_comments_and_strings(source),
                &mut mentioned_names,
            );
            mentioned_names.sort();
            mentioned_names.dedup();
            return graph::FileFacts {
                mentioned_names,
                ..graph::FileFacts::default()
            };
        }
        return graph::FileFacts::default();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar.language()).is_err() {
        return graph::FileFacts::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return graph::FileFacts::default();
    };
    let root_node = tree.root_node();
    let relative_path_buf = PathBuf::from(relative);

    let mut symbols = crate::index::extract_symbols(
        grammar,
        language.clone(),
        &relative_path_buf,
        source,
        root_node,
    );
    // googletest and its kin declare a test as a macro, `TEST(Suite, Name)`,
    // which parses as a function called `TEST`. Name each case for what it
    // is before any call is attributed to it.
    let declared_tests = if matches!(language.id(), "c" | "cpp") {
        cfamily::name_declared_tests(&mut symbols, root_node, source, relative)
    } else {
        Vec::new()
    };
    let file_entrypoints =
        entrypoints::extract_from_tree(language, &relative_path_buf, source, root_node);
    let raw_calls = crate::index::extract_calls(grammar, &relative_path_buf, source, root_node);

    // Attribute each call to the innermost symbol whose byte range contains it.
    let enclosing = |start: usize, end: usize| {
        symbols
            .iter()
            .filter(|symbol| symbol.byte_range.start <= start && symbol.byte_range.end >= end)
            .min_by_key(|symbol| symbol.byte_range.end - symbol.byte_range.start)
    };
    // Python's `if __name__ == "__main__":` block is a startup path that lives
    // outside every function. Calls made there belong to that entrypoint;
    // dropping them stranded `main()` and everything it calls.
    let main_blocks = entrypoints::python_main_blocks(language, source, root_node);
    let caller_of = |start: usize, end: usize| -> (Option<String>, Option<String>) {
        match enclosing(start, end) {
            Some(symbol) => (
                Some(symbol.name.clone()),
                Some(symbol.enclosing_scope.clone()),
            ),
            None if main_blocks
                .iter()
                .any(|(block_start, block_end)| *block_start <= start && end <= *block_end) =>
            {
                (Some(graph::MODULE_MAIN_CALLER.to_owned()), None)
            }
            None => (None, None),
        }
    };

    // Where an assertion is called, so every symbol enclosing it can be
    // credited: an assertion in a callback a test defines is the test's.
    let mut assertion_spans: Vec<(usize, usize)> = Vec::new();
    let mut calls: Vec<graph::CallSite> = raw_calls
        .into_iter()
        .map(|call| {
            let (caller, caller_scope) = caller_of(call.byte_range.start, call.byte_range.end);
            let (callee, qualifier) = split_callee(&call.raw_callee);
            if testing::is_assertion_name(&callee) {
                assertion_spans.push((call.byte_range.start, call.byte_range.end));
            }
            graph::CallSite {
                caller,
                callee,
                qualifier,
                caller_scope,
                file: relative.to_owned(),
                is_reference: false,
            }
        })
        .collect();

    // Calls hidden inside macro bodies. `writeln!(w, "{}", plural(n))` contains
    // a real call to `plural`, but the grammar sees only an opaque token tree,
    // so the call graph would break there.
    for (name, start, end) in macro_call_sites(root_node, source) {
        let (caller, caller_scope) = caller_of(start, end);
        if testing::is_assertion_name(&name) {
            assertion_spans.push((start, end));
        }
        calls.push(graph::CallSite {
            caller,
            callee: name,
            qualifier: None,
            caller_scope,
            file: relative.to_owned(),
            is_reference: false,
        });
    }

    // Types are used in annotations, not called. `booking_data: BookingCreate`
    // is a real use of BookingCreate, but no call appears anywhere — so without
    // this every Pydantic model and TypeScript interface reads as dead.
    for (name, start, end) in type_reference_sites(root_node, source) {
        let (caller, caller_scope) = caller_of(start, end);
        calls.push(graph::CallSite {
            caller,
            callee: name,
            qualifier: None,
            caller_scope,
            file: relative.to_owned(),
            is_reference: true,
        });
    }

    // Rust attributes name functions in strings: `#[serde(default = "f")]`,
    // `serialize_with = "path::f"`, `skip_serializing_if = "Vec::is_empty"`.
    // The derive calls them; the source never does. Each is a use by the item
    // the attribute decorates.
    let rust = if language.id() == "rust" {
        rust_facts::extract(root_node, source)
    } else {
        rust_facts::RustFacts::default()
    };
    let mut attribute_names: Vec<String> = Vec::new();
    for (path, start, end) in &rust.attribute_paths {
        let (callee, qualifier) = split_callee(path);
        let (caller, caller_scope) = caller_of(*start, *end);
        attribute_names.extend(path.split("::").map(str::to_owned));
        calls.push(graph::CallSite {
            caller,
            callee,
            qualifier,
            caller_scope,
            file: relative.to_owned(),
            is_reference: true,
        });
    }

    // JSX: components are rendered, not called, and React Router declares its
    // routes as markup. Both are invisible to call extraction.
    let jsx_facts = jsx::extract(language, &relative_path_buf, source, root_node);
    let mut file_entrypoints = file_entrypoints;
    file_entrypoints.extend(jsx_facts.entrypoints);
    calls.extend(jsx::renders_as_calls(
        &jsx_facts.renders,
        relative,
        |start, end| {
            enclosing(start, end)
                .map(|symbol| (symbol.name.clone(), symbol.enclosing_scope.clone()))
        },
    ));

    let mut mentioned_names = Vec::new();
    collect_opaque_identifiers(root_node, source, &mut mentioned_names);
    // Anything wearing a decorator is invoked by a framework, not by the source.
    // Pydantic `@validator`, FastAPI `@app.exception_handler`, and
    // `@app.on_event("shutdown")` all register callbacks that nothing in the
    // repository calls. Reporting them as dead code is a false accusation.
    mentioned_names.extend(decorated_function_names(root_node, source));
    // `__all__ = ["parse", "to_csv"]` is Python declaring its public API, and a
    // package re-exporting a name in `__init__.py` is publishing it. Nothing in
    // the repository need call such a name for it to be in use.
    mentioned_names.extend(explicitly_exported_names(root_node, source));
    // A method declared in a trait or interface is dispatched to through that
    // contract, and the call site names the contract rather than any one
    // implementation. The declaration is therefore evidence that every
    // implementation of it is reachable, even when nothing in this repository
    // calls it directly - the whole point of publishing an interface is that
    // the caller may live elsewhere.
    mentioned_names.extend(contract_method_names(root_node, source));
    // The shell runs whatever a word expands to, so every word is a possible
    // command.
    if language.id() == "shell" {
        cfamily::shell_words(root_node, source, &mut mentioned_names);
        mentioned_names.sort();
        mentioned_names.dedup();
    }
    // C and C++ reach functions through addresses, tables, macro bodies,
    // overrides and operators, none of which is a call expression. Every use
    // of a name outside its own declaration is therefore a mention of it.
    let mut declaration_sites: Vec<(String, usize)> = Vec::new();
    if matches!(language.id(), "c" | "cpp") {
        cfamily::reference_names(root_node, source, &mut mentioned_names, &mut declaration_sites);
        cfamily::implicitly_invoked_names(language.id(), root_node, source, &mut mentioned_names);
        mentioned_names.sort();
        mentioned_names.dedup();
    }
    mentioned_names.extend(attribute_names);

    // Methods in `impl Trait for Type` are invoked through the trait — by
    // generics, operators, derives, or a crate outside this repository — so the
    // absence of a visible call proves nothing about them.
    let symbol_at: HashMap<(usize, usize), &Symbol> = symbols
        .iter()
        .map(|symbol| ((symbol.byte_range.start, symbol.byte_range.end), symbol))
        .collect();
    let symbol_id_at = |start: usize, end: usize| {
        symbol_at
            .get(&(start, end))
            .map(|symbol| graph::function_id(relative, &symbol.name, symbol.line_range.start))
    };
    let dispatched: Vec<graph::DispatchedMethod> = rust
        .trait_impl_methods
        .iter()
        .filter_map(|(type_name, start, end)| {
            Some(graph::DispatchedMethod {
                type_name: type_name.clone(),
                method_id: symbol_id_at(*start, *end)?,
            })
        })
        .collect();

    // Rust says which functions are tests with an attribute; the name is free.
    let test_functions: Vec<String> = rust
        .test_functions
        .iter()
        .filter_map(|(start, end, _)| symbol_id_at(*start, *end))
        .collect();

    // Which symbols can fail. A call to an assertion function counts, and so
    // do the forms that are not calls at all: Python's `assert` statement,
    // Rust's `assert!` / `panic!` macros, and raising or throwing. Every
    // enclosing symbol is credited, not only the innermost: an assertion in a
    // callback a test hands to a helper still checks that test.
    assertion_spans.extend(assertion_statements(root_node, source));
    let mut asserting_symbols: Vec<String> = Vec::new();
    for (start, end) in assertion_spans {
        for symbol in symbols
            .iter()
            .filter(|symbol| symbol.byte_range.start <= start && symbol.byte_range.end >= end)
        {
            asserting_symbols.push(graph::function_id_for(relative, &symbol.name));
        }
    }
    // `#[should_panic]` is the check: the test fails unless the body panics.
    for (start, end, should_panic) in &rust.test_functions {
        if let Some(symbol) = symbol_at.get(&(*start, *end)).filter(|_| *should_panic) {
            asserting_symbols.push(graph::function_id_for(relative, &symbol.name));
        }
    }
    asserting_symbols.sort();
    asserting_symbols.dedup();

    // Rust marks test code with an attribute, not a name. Matching on the
    // module being called `tests` is a guess that breaks the moment someone
    // writes `mod live_tests`; the attribute is the language's own rule.
    for (start, end) in test_marker_ranges(language, root_node, source) {
        for symbol in symbols.iter_mut() {
            if symbol.byte_range.start >= start && symbol.byte_range.end <= end {
                symbol.enclosing_scope = "tests".to_owned();
            }
        }
    }

    let docstrings = claims::collect_docstrings(language, relative, source, root_node);

    graph::FileFacts {
        symbols,
        entrypoints: file_entrypoints,
        calls,
        mentioned_names,
        asserting_symbols,
        docstrings,
        declared_tests,
        dispatched,
        test_functions,
        declaration_sites,
    }
}

/// Find names used in type positions.
///
/// A type annotation is a use of the type. Python's `def create(data: Booking)`,
/// TypeScript's `const u: User`, and a Rust `-> Result<Booking>` all reference a
/// declared type without calling anything.
fn type_reference_sites(
    root: tree_sitter::Node<'_>,
    source: &[u8],
) -> Vec<(String, usize, usize)> {
    let mut found = Vec::new();
    let mut stack = vec![root];

    while let Some(current) = stack.pop() {
        let kind = current.kind();
        let is_type_position = kind == "type"
            || kind == "type_annotation"
            || kind == "type_identifier"
            || kind == "generic_type"
            || kind == "constrained_type"
            || kind == "union_type"
            || kind == "type_parameter"
            || kind == "type_arguments"
            || kind == "base_class_clause"
            || kind == "argument_list" && current.parent().map(|p| p.kind()) == Some("class_definition")
            || kind == "superclass"
            || kind == "class_heritage"
            || kind == "extends_clause"
            || kind == "implements_clause"
            || kind == "return_type";

        if is_type_position {
            collect_type_identifiers(current, source, &mut found);
            continue;
        }

        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    found.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    found.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
    found
}

fn collect_type_identifiers(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    found: &mut Vec<(String, usize, usize)>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let kind = current.kind();
        if kind == "identifier" || kind == "type_identifier" || kind == "constant" {
            if let Some(bytes) = source.get(current.byte_range()) {
                let text = String::from_utf8_lossy(bytes).trim().to_owned();
                if !text.is_empty() {
                    found.push((text, current.start_byte(), current.end_byte()));
                }
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Names of functions that carry a decorator or attribute annotation.
fn decorated_function_names(root: tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root];

    while let Some(current) = stack.pop() {
        if current.kind() == "decorated_definition" {
            if let Some(definition) = current.child_by_field_name("definition") {
                if let Some(name) = definition.child_by_field_name("name") {
                    if let Some(bytes) = source.get(name.byte_range()) {
                        let text = String::from_utf8_lossy(bytes).trim().to_owned();
                        if !text.is_empty() {
                            found.push(text);
                        }
                    }
                }
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    found
}

/// Assertion forms that are not function calls: Python's `assert` statement and
/// Rust's `assert!`/`panic!`/`unreachable!` macros.
fn assertion_statements(root: tree_sitter::Node<'_>, source: &[u8]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut stack = vec![root];

    while let Some(current) = stack.pop() {
        let kind = current.kind();
        // Raising or throwing fails the test that reaches it.
        if kind == "assert_statement"
            || kind == "assert"
            || kind == "raise_statement"
            || kind == "throw_statement"
            || kind == "throw_expression"
        {
            found.push((current.start_byte(), current.end_byte()));
        } else if kind == "try_expression" && propagates_to_function(current) {
            // `?` returns the error from the function. In a test that returns
            // `Result`, that error fails the test, so `?` is a check.
            found.push((current.start_byte(), current.end_byte()));
        } else if kind.contains("macro_invocation") {
            let text = source
                .get(current.byte_range())
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default();
            let name = text.split('!').next().unwrap_or("").trim();
            if testing::is_assertion_name(name) || name == "panic" || name == "unreachable" {
                found.push((current.start_byte(), current.end_byte()));
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    found
}

/// Does a Rust `?` return from the enclosing function, rather than from a
/// closure or async block nested inside it?
fn propagates_to_function(node: tree_sitter::Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(candidate) = current {
        match candidate.kind() {
            "function_item" => return true,
            "closure_expression" | "async_block" | "try_block" => return false,
            _ => current = candidate.parent(),
        }
    }
    false
}

/// Find call sites inside macro token trees.
///
/// Inside a token tree the grammar gives us a flat stream of tokens rather than
/// expressions, so a call is recognised structurally: an identifier immediately
/// followed by a parenthesised group. That is exactly the shape of `plural(n)`
/// inside `writeln!(...)`, and it recovers edges that would otherwise be lost.
fn macro_call_sites(
    root: tree_sitter::Node<'_>,
    source: &[u8],
) -> Vec<(String, usize, usize)> {
    let mut found = Vec::new();
    let mut stack = vec![root];

    while let Some(current) = stack.pop() {
        let kind = current.kind();
        if kind.contains("macro") || kind == "token_tree" {
            collect_token_tree_calls(current, source, &mut found);
            continue;
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    found
}

fn collect_token_tree_calls(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    found: &mut Vec<(String, usize, usize)>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        let children: Vec<tree_sitter::Node<'_>> = current.children(&mut cursor).collect();
        for (position, child) in children.iter().enumerate() {
            if child.kind().contains("identifier") {
                let followed_by_group = children
                    .get(position + 1)
                    .map(|next| next.kind() == "token_tree" || next.kind() == "(")
                    .unwrap_or(false);
                if followed_by_group {
                    if let Some(bytes) = source.get(child.byte_range()) {
                        let text = String::from_utf8_lossy(bytes).trim().to_owned();
                        if !text.is_empty() {
                            found.push((text, child.start_byte(), child.end_byte()));
                        }
                    }
                }
            }
            stack.push(*child);
        }
    }
}

/// Method names the language runtime or a framework invokes, never the author.
///
/// `Display::fmt` is called by every `{}` in a format string; `Drop::drop` by
/// the compiler. Nothing in the source "calls" them, so a pure call-graph view
/// reports them as dead. They are not.
const RUNTIME_INVOKED_NAMES: &[&str] = &[
    "fmt", "drop", "clone", "default", "eq", "ne", "hash", "cmp", "partial_cmp", "next", "poll",
    "deref", "deref_mut", "from", "into", "try_from", "try_into", "as_ref", "as_mut", "index",
    "add", "sub", "mul", "div", "neg", "not", "serialize", "deserialize", "to_string",
    "__init__", "__str__", "__repr__", "__enter__", "__exit__", "__call__", "__iter__",
    "toString", "valueOf", "constructor", "equals", "hashCode", "compareTo", "Dispose",
];

/// Collect identifiers from regions where a call cannot be proven but is
/// plausible: macro token trees, plus the runtime-invoked names above.
fn collect_opaque_identifiers(node: tree_sitter::Node<'_>, source: &[u8], names: &mut Vec<String>) {
    let mut stack = vec![node];

    while let Some(current) = stack.pop() {
        let kind = current.kind();
        // Macro bodies hide real calls behind an opaque token tree, and an
        // identifier in an argument position is usually a function being passed
        // as a value (`.map(clean_path)`) rather than a call we can see.
        if kind.contains("macro")
            || kind == "token_tree"
            || kind.contains("argument")
            || kind == "arguments"
        {
            collect_identifier_text(current, source, names);
            continue;
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    for name in RUNTIME_INVOKED_NAMES {
        names.push((*name).to_owned());
    }
    names.sort();
    names.dedup();
}

fn collect_identifier_text(node: tree_sitter::Node<'_>, source: &[u8], names: &mut Vec<String>) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind().contains("identifier") || current.kind() == "word" {
            if let Some(bytes) = source.get(current.byte_range()) {
                let text = String::from_utf8_lossy(bytes).trim().to_owned();
                if !text.is_empty() {
                    names.push(text);
                }
            }
        }
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Split a callee expression into its name and the segment that qualifies it.
///
/// `cli::run` -> (`run`, Some(`cli`)); `utils.hash` -> (`hash`, Some(`utils`));
/// `hash` -> (`hash`, None). The qualifier is what disambiguates a name that
/// several files define.
fn split_callee(raw: &str) -> (String, Option<String>) {
    // Generic arguments are not part of the name: `test_decode::<Receipt<T>>`
    // calls `test_decode`, and `Vec::<u8>::new` is `Vec::new`. Left in, the
    // `>` split turned the type argument into the callee and the call never
    // resolved.
    let stripped = strip_generic_arguments(raw);
    let raw = stripped.as_str();
    let segments: Vec<&str> = raw
        .split(['.', ':', '-', '>'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect();
    let name = segments.last().copied().unwrap_or(raw).to_owned();
    let qualifier = if segments.len() >= 2 {
        segments
            .get(segments.len() - 2)
            .map(|segment| (*segment).to_owned())
    } else {
        None
    };
    (name, qualifier)
}

/// Remove balanced `<...>` groups. A `>` with no open `<` is kept, so the
/// C/C++ `ptr->method` arrow survives.
fn strip_generic_arguments(raw: &str) -> String {
    if !raw.contains('<') {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    let mut depth = 0usize;
    for character in raw.chars() {
        match character {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            _ => out.push(character),
        }
    }
    out
}

/// A file after indexing, with the facts the graph needs.
#[derive(Debug, Clone)]
pub struct IndexedFile {
    pub relative: String,
    pub language: SourceLanguage,
    pub line_count: usize,
    pub symbol_count: usize,
}

pub fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn line_count(source: &[u8]) -> usize {
    if source.is_empty() {
        return 0;
    }
    let newlines = source.iter().filter(|byte| **byte == b'\n').count();
    // A trailing newline terminates the last line rather than starting another.
    // Counting it as one more made every file that ends properly — which is
    // nearly all of them — report a line range one longer than the file.
    if source.last() == Some(&b'\n') {
        newlines
    } else {
        newlines + 1
    }
}

/// Paths that are tests. Test code is verification evidence, never a product feature.
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("_test.")
        || lower.starts_with("test_")
        || lower.contains("/test_")
        || lower.ends_with("_test.go")
        || lower.contains("/conftest.py")
        // Fixtures exist only to serve tests, so they are test material even
        // though nothing in the product calls them.
        || lower.starts_with("fixtures/")
        || lower.contains("/fixtures/")
        || lower.starts_with("testdata/")
        || lower.contains("/testdata/")
        // Layouts the build tool enforces: Maven and Gradle put tests under
        // src/test, testthat under tests/testthat, RSpec under spec.
        || lower.contains("/src/test/")
        || lower.starts_with("src/test/")
        || lower.contains("/testthat/")
        || lower.ends_with("_spec.rb")
        || lower.ends_with("_test.rb")
        || lower.ends_with("test.php")
        || lower.ends_with("tests.cs")
        || lower.ends_with("test.cs")
        || lower.ends_with("test.java")
        || lower.ends_with("tests.java")
        || lower.ends_with("_test.cc")
        || lower.ends_with("_test.cpp")
        || lower.ends_with("_test.exs")
        || lower.ends_with(".t.sol")
        || lower.contains("/__mocks__/")
}

/// Generated or vendored code that nobody wrote and nobody should have to explain.
pub fn is_generated_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with("dist/")
        || lower.contains("/dist/")
        || lower.starts_with("build/")
        || lower.contains("/build/")
        || lower.starts_with("vendor/")
        || lower.contains("/vendor/")
        || lower.starts_with("generated/")
        || lower.contains("/generated/")
        || lower.contains("/__generated__/")
        || lower.starts_with("target/")
        || lower.contains("/target/")
        // Coverage reporters emit instrumented copies of the source.
        || lower.starts_with("coverage/")
        || lower.contains("/coverage/")
        || lower.contains("/.next/")
        || lower.contains("/out/")
        || lower.contains(".generated.")
        || lower.contains(".min.")
}

/// Configuration and manifest files: they shape behaviour but are not features.
pub fn is_config_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    matches!(
        name,
        "package.json"
            | "cargo.toml"
            | "pyproject.toml"
            | "tsconfig.json"
            | "dockerfile"
            | "docker-compose.yml"
            | "docker-compose.yaml"
            | "makefile"
            | "go.mod"
            | "requirements.txt"
    ) || lower.contains("/config/")
        || lower.starts_with("config/")
        || name.contains(".config.")
}

/// Documentation carries promises, not implementations.
pub fn is_documentation_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".mdx") || lower.ends_with(".rst")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a snippet so the tree-walking extractors can be tested directly.
    fn parse(language_id: &str, source: &[u8]) -> (SourceLanguage, tree_sitter::Tree) {
        let language = SourceLanguage::new(language_id).expect("language");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language.tree_sitter_language().expect("grammar"))
            .expect("set language");
        let tree = parser.parse(source, None).expect("parse");
        (language, tree)
    }

    #[test]
    fn splits_a_callee_into_its_name_and_qualifier() {
        // The qualifier is what breaks a tie when several files define a name,
        // so losing it silently severs reachability chains.
        assert_eq!(
            split_callee("auth_controller.login"),
            ("login".to_owned(), Some("auth_controller".to_owned()))
        );
        assert_eq!(
            split_callee("cli::run"),
            ("run".to_owned(), Some("cli".to_owned()))
        );
        assert_eq!(split_callee("hash"), ("hash".to_owned(), None));
        // C++ template arguments are not part of the name.
        assert_eq!(
            split_callee("splat_pawn_moves<Up + Up>"),
            ("splat_pawn_moves".to_owned(), None)
        );
        assert_eq!(
            split_callee("worker->search<NonPV>"),
            ("search".to_owned(), Some("worker".to_owned()))
        );
        // Turbofish and generic arguments are not the callee.
        assert_eq!(
            split_callee("reth_codecs::test_utils::test_decode::<Receipt<TxType>>"),
            ("test_decode".to_owned(), Some("test_utils".to_owned()))
        );
        assert_eq!(
            split_callee("Vec::<u8>::new"),
            ("new".to_owned(), Some("Vec".to_owned()))
        );
        assert_eq!(
            split_callee("node->next"),
            ("next".to_owned(), Some("node".to_owned()))
        );
        // Deeply qualified names keep only the segment that identifies the file.
        assert_eq!(
            split_callee("crate::audit::graph::build"),
            ("build".to_owned(), Some("graph".to_owned()))
        );
    }

    #[test]
    fn a_type_annotation_counts_as_a_use_of_the_type() {
        // Without this every Pydantic model and TypeScript interface reads as
        // dead: they are named in annotations and never called.
        let source = b"def create(data: BookingCreate) -> BookingResponse:\n    return None\n";
        let (_, tree) = parse("python", source);
        let found = type_reference_sites(tree.root_node(), source);
        let names: Vec<&str> = found.iter().map(|(name, _, _)| name.as_str()).collect();

        assert!(names.contains(&"BookingCreate"), "got {names:?}");
        assert!(names.contains(&"BookingResponse"), "got {names:?}");
    }

    #[test]
    fn a_decorated_function_is_framework_invoked_not_dead() {
        // Pydantic validators and FastAPI exception handlers are called by the
        // framework; nothing in the repository calls them by name.
        let source = b"@validator('email')\ndef check_email(cls, value):\n    return value\n";
        let (_, tree) = parse("python", source);
        let names = decorated_function_names(tree.root_node(), source);

        assert!(names.contains(&"check_email".to_owned()), "got {names:?}");
    }

    #[test]
    fn a_call_inside_a_macro_is_still_a_call() {
        // `writeln!(w, "{}", plural(n))` calls plural, but the grammar sees an
        // opaque token tree, so the call graph would break there.
        let source = b"fn show(n: usize) {\n    println!(\"{}\", plural(n));\n}\n";
        let (_, tree) = parse("rust", source);
        let found = macro_call_sites(tree.root_node(), source);
        let names: Vec<&str> = found.iter().map(|(name, _, _)| name.as_str()).collect();

        assert!(names.contains(&"plural"), "got {names:?}");
    }

    #[test]
    fn assertions_that_are_not_calls_are_still_assertions() {
        // Python's `assert` is a statement and Rust's `assert!` is a macro.
        // Missing either would report a real test as one that checks nothing.
        let python = b"def test_it():\n    assert 1 == 1\n";
        let (_, tree) = parse("python", python);
        assert!(
            !assertion_statements(tree.root_node(), python).is_empty(),
            "python assert statement"
        );

        let rust = b"fn test_it() {\n    assert_eq!(1, 1);\n}\n";
        let (_, tree) = parse("rust", rust);
        assert!(
            !assertion_statements(tree.root_node(), rust).is_empty(),
            "rust assert macro"
        );

        // A test that only prints has nothing to find.
        let silent = b"def test_it():\n    print('ok')\n";
        let (_, tree) = parse("python", silent);
        assert!(assertion_statements(tree.root_node(), silent).is_empty());
    }

    #[test]
    fn cfg_test_is_read_as_evidence_not_guessed_from_the_module_name() {
        // Matching on a module called `tests` is a habit people can break. The
        // attribute is the language's own rule, so the name may be anything.
        let source = br#"
#[cfg(test)]
mod zqx_whatever {
    fn helper() -> u8 { 1 }
}

fn production() -> u8 { 2 }
"#;
        let (_, tree) = parse("rust", source);
        let language = SourceLanguage::new("rust").expect("rust");
        let ranges = test_marker_ranges(&language, tree.root_node(), source);

        assert_eq!(ranges.len(), 1, "the cfg(test) block should be found");
        let (start, end) = ranges[0];
        let block = String::from_utf8_lossy(&source[start..end]);
        assert!(block.contains("helper"), "block covers the test helper");
        assert!(
            !block.contains("production"),
            "the range must stop at the test module"
        );
    }

    #[test]
    fn line_count_matches_the_file() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"one"), 1);
        assert_eq!(line_count(b"one\ntwo"), 2);
        assert_eq!(line_count(b"one\ntwo\n"), 2);
    }

    #[test]
    fn classifies_test_paths() {
        assert!(is_test_path("src/auth/login.test.ts"));
        assert!(is_test_path("tests/test_login.py"));
        assert!(is_test_path("internal/server_test.go"));
        assert!(!is_test_path("src/auth/login.ts"));
        // "latest" contains "test" but is not a test path.
        assert!(!is_test_path("src/latest/index.ts"));
    }

    #[test]
    fn classifies_generated_and_config_paths() {
        assert!(is_generated_path("dist/bundle.js"));
        assert!(is_generated_path("target/debug/thing.rs"));
        assert!(is_config_path("package.json"));
        assert!(is_config_path("Dockerfile"));
        assert!(!is_config_path("src/server.ts"));
    }

    #[test]
    fn entrypoint_ids_are_stable_and_unique_per_line() {
        let entrypoint = EntryPoint {
            kind: EntryPointKind::Http,
            name: "GET /users".to_owned(),
            exposure: Exposure::External,
            http_method: Some("GET".to_owned()),
            route: Some("/users".to_owned()),
            handler_name: None,
            file: PathBuf::from("server.js"),
            line_range: LineRange { start: 12, end: 12 },
            framework: "http".to_owned(),
        };
        assert_eq!(
            entrypoint.id("server.js"),
            "entrypoint:http:server.js:12:GET /users"
        );
    }
}

/// Names a module explicitly publishes.
///
/// Python has no `export` keyword, so `__all__` is the declaration, and a
/// re-export in `__init__.py` is a package saying "this is our API". Neither
/// produces a call site, so without this a published function looks unused.
fn explicitly_exported_names(root: tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root];

    while let Some(current) = stack.pop() {
        if current.kind() == "assignment" {
            let is_all = current
                .child_by_field_name("left")
                .and_then(|left| source.get(left.byte_range()))
                .map(|bytes| String::from_utf8_lossy(bytes).trim() == "__all__")
                .unwrap_or(false);
            if is_all {
                if let Some(right) = current.child_by_field_name("right") {
                    collect_string_literals(right, source, &mut found);
                }
                continue;
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    found
}

fn collect_string_literals(node: tree_sitter::Node<'_>, source: &[u8], found: &mut Vec<String>) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind().contains("string") {
            if let Some(bytes) = source.get(current.byte_range()) {
                let text = String::from_utf8_lossy(bytes);
                let trimmed = text.trim().trim_matches(['"', '\'']);
                if !trimmed.is_empty() {
                    found.push(trimmed.to_owned());
                }
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}


/// Markers that a language or its test runner treats as declaring test code.
///
/// Each is a rule the tooling enforces, not a habit: JUnit only runs `@Test`,
/// xUnit only runs `[Fact]`, Rust only compiles `#[cfg(test)]` under `cargo
/// test`. Matching these is evidence in a way that matching a module someone
/// happens to call `tests` never is.
const TEST_MARKERS: &[&str] = &[
    // Rust
    "cfg(test)",
    "#[test]",
    "#[bench]",
    // Java / Kotlin — JUnit, TestNG
    "@Test",
    "@ParameterizedTest",
    "@RepeatedTest",
    "@BeforeEach",
    "@BeforeAll",
    "@AfterEach",
    // C# — NUnit, xUnit, MSTest
    "[Test]",
    "[TestCase",
    "[TestMethod]",
    "[Fact]",
    "[Theory]",
    "[SetUp]",
    // PHP — PHPUnit attribute and docblock forms
    "#[Test]",
    "@test",
    // Python — pytest decorators on test helpers
    "@pytest.fixture",
    "@pytest.mark",
];

/// Byte ranges of code a language marks as test code.
///
/// Anything inside one of these ranges is test code whatever it is named. This
/// replaces guessing from a module name, which broke the moment a module was
/// called `live_tests`.
fn test_marker_ranges(
    language: &SourceLanguage,
    root: tree_sitter::Node<'_>,
    source: &[u8],
) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut stack = vec![root];

    while let Some(current) = stack.pop() {
        // Go states it in the signature: a test takes *testing.T or *testing.B.
        // No annotation exists, so the parameter list is the marker.
        if language.id() == "go" && current.kind() == "function_declaration" {
            if let Some(parameters) = current.child_by_field_name("parameters") {
                let text = node_source_text(parameters, source);
                if text.contains("testing.T") || text.contains("testing.B") {
                    found.push((current.start_byte(), current.end_byte()));
                }
            }
        }

        // C and C++ have no attribute; googletest declares tests as a macro.
        if matches!(language.id(), "c" | "cpp")
            && (current.kind() == "function_definition" || current.kind().contains("call_expression"))
        {
            let text = node_source_text(current, source);
            let trimmed = text.trim_start();
            if cfamily::TEST_MACROS
                .iter()
                .any(|name| trimmed.strip_prefix(name).is_some_and(|rest| rest.trim_start().starts_with('(')))
            {
                found.push((current.start_byte(), current.end_byte()));
            }
        }

        let mut cursor = current.walk();
        let children: Vec<tree_sitter::Node<'_>> = current.children(&mut cursor).collect();
        for (position, child) in children.iter().enumerate() {
            let kind = child.kind();
            let is_marker_node = kind == "attribute_item"
                || kind == "attribute_list"
                || kind == "annotation"
                || kind == "marker_annotation"
                || kind == "decorator"
                || kind == "modifiers"
                || kind == "comment";
            if !is_marker_node {
                continue;
            }

            let text = node_source_text(*child, source).replace(char::is_whitespace, "");
            if !TEST_MARKERS.iter().any(|marker| {
                text.contains(&marker.replace(char::is_whitespace, ""))
            }) {
                continue;
            }

            // The marked item is this node's own parent when the marker is a
            // modifier list, and the next sibling when it stands alone.
            let target = if kind == "modifiers" || kind == "attribute_list" {
                Some(current)
            } else {
                children.get(position + 1).copied()
            };
            if let Some(target) = target {
                found.push((target.start_byte(), target.end_byte()));
            }
        }

        for child in children {
            stack.push(child);
        }
    }

    found
}

fn node_source_text(node: tree_sitter::Node<'_>, source: &[u8]) -> String {
    source
        .get(node.byte_range())
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

/// Names of methods declared inside a trait, interface, protocol, or abstract
/// type — the contracts other code dispatches through.
///
/// Rust `trait`, TypeScript/Java/C# `interface`, Go `type T interface`, PHP
/// `interface`, Swift `protocol`. Collecting the declared names keeps every
/// implementation of them out of the dead-code findings.
fn contract_method_names(root: tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    const CONTRACT_KINDS: &[&str] = &[
        "trait_item",
        "interface_declaration",
        "interface_type",
        "interface_body",
        "protocol_declaration",
        "abstract_class_declaration",
    ];
    const SIGNATURE_KINDS: &[&str] = &[
        "function_signature",
        "function_signature_item",
        "method_signature",
        "function_item",
        "method_spec",
        "method_elem",
        "method_declaration",
        "abstract_method_signature",
        "property_signature",
        "function_declaration",
    ];

    let mut names = Vec::new();
    let mut stack = vec![(root, false)];
    while let Some((node, inside_contract)) = stack.pop() {
        let here = inside_contract || CONTRACT_KINDS.contains(&node.kind());
        if here && SIGNATURE_KINDS.contains(&node.kind()) {
            if let Some(name) = node.child_by_field_name("name") {
                if let Some(bytes) = source.get(name.byte_range()) {
                    let text = String::from_utf8_lossy(bytes).trim().to_owned();
                    if !text.is_empty() {
                        names.push(text);
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push((child, here));
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Stage timing, printed to stderr when `CODIE_TIMING` is set.
///
/// Performance claims about this pipeline were previously derived by
/// subtracting the runtime of `index` from the runtime of `audit`. That is not
/// sound — the two commands do different amounts of work, and on a warm cache
/// `index` was measured slower than a full `audit`. Timing the stages in place
/// is the only way to say honestly where the time goes.
struct StageTimer {
    enabled: bool,
    started: std::time::Instant,
    last: std::time::Instant,
}

impl StageTimer {
    fn start() -> Self {
        let now = std::time::Instant::now();
        Self {
            enabled: std::env::var_os("CODIE_TIMING").is_some(),
            started: now,
            last: now,
        }
    }

    fn mark(&mut self, stage: &str) {
        let now = std::time::Instant::now();
        if self.enabled {
            eprintln!(
                "[timing] {stage:<24} {:>8.1} ms",
                now.duration_since(self.last).as_secs_f64() * 1000.0
            );
        }
        self.last = now;
    }

    fn total(&self) {
        if self.enabled {
            eprintln!(
                "[timing] {:<24} {:>8.1} ms",
                "TOTAL",
                self.started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}
