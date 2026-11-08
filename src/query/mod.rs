use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tree_sitter::{Parser, Tree};

use crate::cache::{CacheError, CacheStore};
use crate::index::{
    discover_source_files, extract_calls, extract_import_aliases, grammar_for, index_path,
    source_state_hash, ByteRange, IndexError, LineRange, SourceLanguage, Symbol,
};
use crate::llm::{llm_is_configured, provider_from_env, LlmError, LlmProvider, RenderedPrompt};

/// Upper bound on how many candidates are handed to the LLM ranker in one call.
/// Small/medium repos send every function; only very large trees are pre-trimmed
/// by a cheap lexical recall net (never the source of the semantic judgement).
const MAX_LLM_CANDIDATES: usize = 80;
const SNIPPET_PREVIEW_LINES: usize = 8;
const SNIPPET_PREVIEW_LINE_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryResults {
    #[serde(default)]
    root: PathBuf,
    query: String,
    all_requested: bool,
    #[serde(default)]
    retrieval: RetrievalMode,
    #[serde(default)]
    confidence: ConfidenceLevel,
    #[serde(default)]
    suggestions: Vec<String>,
    candidates: Vec<QueryCandidate>,
}

impl QueryResults {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn all_requested(&self) -> bool {
        self.all_requested
    }

    pub fn retrieval(&self) -> RetrievalMode {
        self.retrieval
    }

    pub fn confidence(&self) -> ConfidenceLevel {
        self.confidence
    }

    pub fn suggestions(&self) -> &[String] {
        &self.suggestions
    }

    pub fn candidates(&self) -> &[QueryCandidate] {
        &self.candidates
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetrievalMode {
    /// Resolved purely from the AST/symbol table — no LLM.
    #[default]
    Structural,
    /// Candidates retrieved from the AST and ranked by the LLM.
    Semantic,
}

impl fmt::Display for RetrievalMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Structural => "structural",
            Self::Semantic => "semantic",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfidenceLevel {
    #[default]
    High,
    Medium,
    Low,
}

impl fmt::Display for ConfidenceLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryCandidate {
    pub kind: CandidateKind,
    pub label: String,
    pub file: PathBuf,
    pub line_range: LineRange,
    pub byte_range: ByteRange,
    pub snippet: String,
    /// Ranking confidence in `[0, 1]` for semantic matches; `None` for exact structural hits.
    #[serde(default)]
    pub score: Option<f32>,
    /// Enclosing class/module, shown in pick-lists so the user can choose.
    #[serde(default)]
    pub context: String,
}

impl Eq for QueryCandidate {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateKind {
    FunctionDefinition,
    CallSite,
    EnclosingFunction,
    SemanticMatch,
}

impl fmt::Display for CandidateKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::FunctionDefinition => "function",
            Self::CallSite => "call",
            Self::EnclosingFunction => "containing function",
            Self::SemanticMatch => "match",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Error)]
pub enum QueryError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("failed to canonicalize query path {path}: {source}")]
    CanonicalizePath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read source file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to configure parser for {language}: {message}")]
    ParserLanguage {
        language: SourceLanguage,
        message: String,
    },
    #[error("tree-sitter did not return a parse tree for {path}")]
    ParseReturnedNoTree { path: PathBuf },
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error(transparent)]
    Llm(#[from] LlmError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QueryPlan {
    /// Exact, AST-resolvable phrasing — answered without the LLM.
    Structural(QueryIntent),
    /// Conceptual phrasing — answered by exact-name fast path or LLM ranking.
    Conceptual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QueryIntent {
    FunctionNamed(String),
    CallSites(Vec<CallTarget>),
    FunctionContaining(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CallTarget {
    callee: String,
    require_http_method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CallSite {
    raw_callee: String,
    canonical_callee: String,
    file: PathBuf,
    line_range: LineRange,
    byte_range: ByteRange,
    snippet: String,
}

pub fn find_in_path(path: impl AsRef<Path>, query: &str) -> Result<QueryResults, QueryError> {
    find_in_path_with_llm(path, query, None)
}

/// Run a find, optionally overriding the LLM config with the same `provider,model,key`
/// spec shape used by `CODIE_LLM`.
pub fn find_in_path_with_llm(
    path: impl AsRef<Path>,
    query: &str,
    llm_override: Option<&str>,
) -> Result<QueryResults, QueryError> {
    let requested_root = path.as_ref();
    let root = requested_root
        .canonicalize()
        .map_err(|source| QueryError::CanonicalizePath {
            path: requested_root.to_path_buf(),
            source,
        })?;
    let repo_state_hash = source_state_hash(&root)?;
    let cache = CacheStore::open_for_root(&root)?;

    if let Some(mut results) = cache.get_query_results::<QueryResults>(query, &repo_state_hash)? {
        cache.record_query_hit()?;
        if results.root.as_os_str().is_empty() {
            results.root = root.clone();
            cache.put_query_results(query, &repo_state_hash, &results)?;
        }
        cache.set_last_find_results(&results)?;
        return Ok(results);
    }

    cache.record_query_miss()?;
    let all_requested = query_requests_all(query);

    let results = match plan_query(query) {
        QueryPlan::Structural(intent) => {
            let candidates = run_structural(&root, intent)?;
            if !candidates.is_empty() {
                assemble(
                    &root,
                    query,
                    all_requested,
                    RetrievalMode::Structural,
                    ConfidenceLevel::High,
                    Vec::new(),
                    candidates,
                )
            } else {
                semantic_or_empty(&root, &cache, query, all_requested, llm_override)?
            }
        }
        QueryPlan::Conceptual => {
            // Only treat a *short* query as a bare name. A descriptive sentence that merely
            // contains a word matching some function name must still go to the LLM.
            let exact = if is_bare_name_query(query) {
                exact_name_candidates(&root, query)?
            } else {
                None
            };
            if let Some(candidates) = exact {
                assemble(
                    &root,
                    query,
                    all_requested,
                    RetrievalMode::Structural,
                    ConfidenceLevel::High,
                    Vec::new(),
                    candidates,
                )
            } else {
                let mut ranker = provider_from_env(&root, llm_override)?;
                run_semantic(&root, &cache, query, all_requested, ranker.as_mut())?
            }
        }
    };

    cache.put_query_results(query, &repo_state_hash, &results)?;
    cache.set_last_find_results(&results)?;

    Ok(results)
}

/// Falls back to semantic ranking when a structural query found nothing but an LLM is
/// configured; otherwise returns an empty result rather than calling out to a model.
fn semantic_or_empty(
    root: &Path,
    cache: &CacheStore,
    query: &str,
    all_requested: bool,
    llm_override: Option<&str>,
) -> Result<QueryResults, QueryError> {
    if llm_override.is_some() || llm_is_configured(root) {
        let mut ranker = provider_from_env(root, llm_override)?;
        run_semantic(root, cache, query, all_requested, ranker.as_mut())
    } else {
        Ok(assemble(
            root,
            query,
            all_requested,
            RetrievalMode::Structural,
            ConfidenceLevel::Low,
            Vec::new(),
            Vec::new(),
        ))
    }
}

fn assemble(
    root: &Path,
    query: &str,
    all_requested: bool,
    retrieval: RetrievalMode,
    confidence: ConfidenceLevel,
    suggestions: Vec<String>,
    candidates: Vec<QueryCandidate>,
) -> QueryResults {
    QueryResults {
        root: root.to_path_buf(),
        query: query.to_owned(),
        all_requested,
        retrieval,
        confidence,
        suggestions,
        candidates,
    }
}

fn plan_query(query: &str) -> QueryPlan {
    if let Some(target) = identifier_after_phrase(query, "function containing") {
        return QueryPlan::Structural(QueryIntent::FunctionContaining(target));
    }

    if let Some(target) = identifier_after_phrase(query, "containing function") {
        return QueryPlan::Structural(QueryIntent::FunctionContaining(target));
    }

    if let Some(name) = identifier_after_phrase(query, "function named") {
        return QueryPlan::Structural(QueryIntent::FunctionNamed(name));
    }

    if let Some(name) = identifier_in_where_defined_query(query) {
        return QueryPlan::Structural(QueryIntent::FunctionNamed(name));
    }

    if let Some(name) = identifier_after_phrase(query, "calls to function") {
        return QueryPlan::Structural(QueryIntent::CallSites(vec![CallTarget {
            callee: name,
            require_http_method: None,
        }]));
    }

    if let Some(name) = identifier_after_phrase(query, "call to") {
        return QueryPlan::Structural(QueryIntent::CallSites(vec![CallTarget {
            callee: name,
            require_http_method: None,
        }]));
    }

    if let Some(targets) = call_targets_from_query(query) {
        return QueryPlan::Structural(QueryIntent::CallSites(targets));
    }

    QueryPlan::Conceptual
}

fn run_structural(root: &Path, intent: QueryIntent) -> Result<Vec<QueryCandidate>, QueryError> {
    let mut candidates = match intent {
        QueryIntent::FunctionNamed(name) => find_function_definitions(root, &name)?,
        QueryIntent::CallSites(targets) => find_call_sites(root, &targets)?
            .into_iter()
            .map(candidate_from_call_site)
            .collect(),
        QueryIntent::FunctionContaining(target) => find_containing_functions(root, &target)?,
    };

    candidates.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line_range.start.cmp(&right.line_range.start))
            .then(left.label.cmp(&right.label))
    });

    Ok(candidates)
}

/// A query short enough to be read as "the user typed a function name", not a description.
fn is_bare_name_query(query: &str) -> bool {
    query.split_whitespace().count() <= 3
}

/// For a bare conceptual query, resolve it instantly (no LLM) when a query word is
/// literally a function name in the index.
fn exact_name_candidates(
    root: &Path,
    query: &str,
) -> Result<Option<Vec<QueryCandidate>>, QueryError> {
    let identifiers = identifiers_in_query(query);
    if identifiers.is_empty() {
        return Ok(None);
    }

    let summary = index_path(root)?;
    let mut candidates = Vec::new();
    for symbol in summary.symbols() {
        if identifiers
            .iter()
            .any(|identifier| symbol.name.eq_ignore_ascii_case(identifier))
        {
            candidates.push(candidate_from_symbol(
                symbol,
                CandidateKind::FunctionDefinition,
            )?);
        }
    }

    if candidates.is_empty() {
        return Ok(None);
    }

    candidates.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line_range.start.cmp(&right.line_range.start))
            .then(left.label.cmp(&right.label))
    });
    Ok(Some(candidates))
}

// ---------------------------------------------------------------------------
// Semantic path: AST gathers candidates, the LLM ranks them.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RankInput {
    label: String,
    scope: String,
    file: PathBuf,
    relative_path: String,
    line_range: LineRange,
    byte_range: ByteRange,
    snippet: String,
}

#[derive(Debug, Default, Deserialize)]
struct RankingResponse {
    #[serde(default)]
    confidence: String,
    #[serde(default)]
    matches: Vec<RankingMatch>,
    #[serde(default)]
    suggestions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RankingMatch {
    index: usize,
    #[serde(default)]
    score: Option<f32>,
}

pub(crate) fn run_semantic(
    root: &Path,
    cache: &CacheStore,
    query: &str,
    all_requested: bool,
    ranker: &mut dyn LlmProvider,
) -> Result<QueryResults, QueryError> {
    let inputs = gather_rank_inputs(root, query)?;
    if inputs.is_empty() {
        return Ok(assemble(
            root,
            query,
            all_requested,
            RetrievalMode::Semantic,
            ConfidenceLevel::Low,
            Vec::new(),
            Vec::new(),
        ));
    }

    let prompt = build_ranking_prompt(query, all_requested, &inputs);
    let prompt_hash = prompt.prompt_hash();
    let model_key = ranker.cache_model_key();

    let raw = if let Some(cached) = cache.get_llm_response(&prompt_hash, &model_key)? {
        cached
    } else {
        let response = ranker.complete(&prompt)?;
        cache.put_llm_response(&prompt_hash, &model_key, &response)?;
        response
    };

    let ranking = parse_ranking(&raw);
    Ok(route_ranking(root, query, all_requested, &inputs, ranking))
}

fn gather_rank_inputs(root: &Path, query: &str) -> Result<Vec<RankInput>, QueryError> {
    let summary = index_path(root)?;
    let mut sources: HashMap<PathBuf, Vec<u8>> = HashMap::new();
    let mut inputs = Vec::new();

    for symbol in summary.symbols() {
        if !sources.contains_key(&symbol.file) {
            let source = fs::read(&symbol.file).map_err(|source| QueryError::ReadFile {
                path: symbol.file.clone(),
                source,
            })?;
            sources.insert(symbol.file.clone(), source);
        }
        let source = &sources[&symbol.file];
        let snippet = source_slice(source, symbol.byte_range.start, symbol.byte_range.end);

        inputs.push(RankInput {
            label: symbol.name.clone(),
            scope: symbol.enclosing_scope.clone(),
            relative_path: relative_display_path(root, &symbol.file),
            file: symbol.file.clone(),
            line_range: symbol.line_range,
            byte_range: symbol.byte_range.clone(),
            snippet,
        });
    }

    if inputs.len() > MAX_LLM_CANDIDATES {
        prefilter_inputs(&mut inputs, query);
        inputs.truncate(MAX_LLM_CANDIDATES);
    }

    Ok(inputs)
}

/// Generous lexical recall net used only to cap an oversized candidate list before the
/// LLM does the real ranking. Stable so identical inputs stay deterministic.
fn prefilter_inputs(inputs: &mut [RankInput], query: &str) {
    let terms: Vec<String> = identifiers_in_query(query)
        .into_iter()
        .map(|term| term.to_lowercase())
        .collect();
    if terms.is_empty() {
        return;
    }

    let mut scored: Vec<(usize, usize)> = inputs
        .iter()
        .enumerate()
        .map(|(position, input)| {
            let haystack =
                format!("{} {} {}", input.label, input.scope, input.snippet).to_lowercase();
            let score = terms
                .iter()
                .filter(|term| haystack.contains(term.as_str()))
                .count();
            (position, score)
        })
        .collect();
    scored.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));

    let order: Vec<usize> = scored.into_iter().map(|(position, _)| position).collect();
    apply_permutation(inputs, &order);
}

fn apply_permutation(inputs: &mut [RankInput], order: &[usize]) {
    let reordered: Vec<RankInput> = order.iter().map(|&index| inputs[index].clone()).collect();
    inputs.clone_from_slice(&reordered);
}

fn build_ranking_prompt(query: &str, all_requested: bool, inputs: &[RankInput]) -> RenderedPrompt {
    let system = [
        "You are the ranking step of Code Reader, a code navigation tool.",
        "You are given the user's description and a numbered list of candidate functions already retrieved from the repository by AST search.",
        "Select and rank only the candidates that match the description; choose strictly by their number.",
        "Never invent code or candidates. Do not explain the code, summarize it, or suggest changes — only rank.",
        "Reply with ONLY a JSON object of the form {\"confidence\":\"high|medium|low\",\"matches\":[{\"index\":N,\"score\":0.0}],\"suggestions\":[]}.",
        "Use \"high\" for exactly one clear match, \"medium\" for several plausible matches (list each), and \"low\" when nothing matches well (leave matches empty and put 2-4 short \"did you mean\" directions in suggestions).",
        "score is your confidence in [0,1].",
    ]
    .join(" ");

    let mut user = String::new();
    user.push_str("Query: ");
    user.push_str(query.trim());
    user.push('\n');
    if all_requested {
        user.push_str(
            "Mode: the user asked for ALL/every match (audit). Include every candidate that applies and never drop one.\n",
        );
    }
    user.push_str("\nCandidates:\n");
    for (position, input) in inputs.iter().enumerate() {
        user.push_str(&format!(
            "{}. name={} scope={} file={}:{}\n",
            position + 1,
            input.label,
            input.scope,
            input.relative_path,
            format_line_range(&input.line_range)
        ));
        for line in snippet_preview(&input.snippet) {
            user.push_str("    ");
            user.push_str(&line);
            user.push('\n');
        }
    }

    RenderedPrompt { system, user }
}

fn snippet_preview(snippet: &str) -> Vec<String> {
    snippet
        .lines()
        .take(SNIPPET_PREVIEW_LINES)
        .map(|line| {
            if line.chars().count() > SNIPPET_PREVIEW_LINE_CHARS {
                let truncated: String = line.chars().take(SNIPPET_PREVIEW_LINE_CHARS).collect();
                format!("{truncated}…")
            } else {
                line.to_owned()
            }
        })
        .collect()
}

fn parse_ranking(raw: &str) -> RankingResponse {
    extract_json_object(raw)
        .and_then(|json| serde_json::from_str::<RankingResponse>(json).ok())
        .unwrap_or_default()
}

fn route_ranking(
    root: &Path,
    query: &str,
    all_requested: bool,
    inputs: &[RankInput],
    ranking: RankingResponse,
) -> QueryResults {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for matched in &ranking.matches {
        let Some(index) = matched.index.checked_sub(1) else {
            continue;
        };
        let Some(input) = inputs.get(index) else {
            continue;
        };
        if !seen.insert(index) {
            continue;
        }
        candidates.push(candidate_from_rank_input(input, matched.score));
    }

    let (confidence, suggestions) = if all_requested {
        // Audit exception: surface every match the model returned, never disambiguate.
        (ConfidenceLevel::High, Vec::new())
    } else if candidates.is_empty() {
        let suggestions = if ranking.suggestions.is_empty() {
            fallback_suggestions(inputs)
        } else {
            ranking.suggestions
        };
        (ConfidenceLevel::Low, suggestions)
    } else {
        let level = parse_confidence_level(&ranking.confidence, candidates.len());
        let suggestions = if level == ConfidenceLevel::Low {
            ranking.suggestions
        } else {
            Vec::new()
        };
        (level, suggestions)
    };

    assemble(
        root,
        query,
        all_requested,
        RetrievalMode::Semantic,
        confidence,
        suggestions,
        candidates,
    )
}

fn parse_confidence_level(raw: &str, candidate_count: usize) -> ConfidenceLevel {
    match raw.trim().to_ascii_lowercase().as_str() {
        "high" => ConfidenceLevel::High,
        "low" => ConfidenceLevel::Low,
        "medium" => ConfidenceLevel::Medium,
        // Unstated confidence: one match reads as high, several as a pick-list.
        _ if candidate_count <= 1 => ConfidenceLevel::High,
        _ => ConfidenceLevel::Medium,
    }
}

fn fallback_suggestions(inputs: &[RankInput]) -> Vec<String> {
    inputs
        .iter()
        .take(3)
        .map(|input| {
            if input.scope == "module" {
                format!(
                    "{} ({}:{})",
                    input.label, input.relative_path, input.line_range.start
                )
            } else {
                format!(
                    "{} in {} ({}:{})",
                    input.label, input.scope, input.relative_path, input.line_range.start
                )
            }
        })
        .collect()
}

fn candidate_from_rank_input(input: &RankInput, score: Option<f32>) -> QueryCandidate {
    QueryCandidate {
        kind: CandidateKind::SemanticMatch,
        label: input.label.clone(),
        file: input.file.clone(),
        line_range: input.line_range,
        byte_range: input.byte_range.clone(),
        snippet: input.snippet.clone(),
        score: Some(score.unwrap_or(0.0).clamp(0.0, 1.0)),
        context: input.scope.clone(),
    }
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, &byte) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + offset + 1]);
                }
            }
            _ => {}
        }
    }

    None
}

fn relative_display_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---------------------------------------------------------------------------
// Structural retrieval (unchanged behaviour, no LLM).
// ---------------------------------------------------------------------------

fn find_function_definitions(
    root: &Path,
    function_name: &str,
) -> Result<Vec<QueryCandidate>, QueryError> {
    let summary = index_path(root)?;
    summary
        .symbols()
        .iter()
        .filter(|symbol| names_match(&symbol.name, function_name))
        .map(|symbol| candidate_from_symbol(symbol, CandidateKind::FunctionDefinition))
        .collect()
}

fn find_containing_functions(root: &Path, target: &str) -> Result<Vec<QueryCandidate>, QueryError> {
    let call_targets = vec![CallTarget {
        callee: target.to_owned(),
        require_http_method: None,
    }];
    let call_sites = find_call_sites(root, &call_targets)?;
    let summary = index_path(root)?;
    let symbols_by_file = symbols_by_file(summary.symbols());
    let mut emitted_symbols = HashSet::new();
    let mut candidates = Vec::new();

    for call_site in call_sites {
        let Some(symbols) = symbols_by_file.get(&call_site.file) else {
            continue;
        };
        let Some(symbol) = innermost_symbol_containing(symbols, call_site.byte_range.start) else {
            continue;
        };
        let key = (
            symbol.file.clone(),
            symbol.byte_range.start,
            symbol.byte_range.end,
        );
        if emitted_symbols.insert(key) {
            candidates.push(candidate_from_symbol(
                symbol,
                CandidateKind::EnclosingFunction,
            )?);
        }
    }

    Ok(candidates)
}

fn find_call_sites(root: &Path, targets: &[CallTarget]) -> Result<Vec<CallSite>, QueryError> {
    let mut call_sites = Vec::new();

    for source_file in discover_source_files(root)? {
        let source = fs::read(&source_file.path).map_err(|source| QueryError::ReadFile {
            path: source_file.path.clone(),
            source,
        })?;

        let Some(grammar) = grammar_for(&source_file.language) else {
            continue;
        };

        let tree = parse_tree(
            grammar.language(),
            source_file.language.clone(),
            &source_file.path,
            &source,
        )?;
        let aliases = extract_import_aliases(grammar, &source, tree.root_node());

        for raw_call in extract_calls(grammar, &source_file.path, &source, tree.root_node()) {
            let canonical_callee = canonicalize_callee(&raw_call.raw_callee, &aliases);
            if targets.iter().any(|target| {
                call_matches_target(
                    &raw_call.raw_callee,
                    &canonical_callee,
                    &raw_call.snippet,
                    target,
                )
            }) {
                call_sites.push(CallSite {
                    raw_callee: raw_call.raw_callee,
                    canonical_callee,
                    file: raw_call.file,
                    line_range: raw_call.line_range,
                    byte_range: raw_call.byte_range,
                    snippet: raw_call.snippet,
                });
            }
        }
    }

    Ok(call_sites)
}

fn parse_tree(
    tree_sitter_language: tree_sitter::Language,
    language: SourceLanguage,
    path: &Path,
    source: &[u8],
) -> Result<Tree, QueryError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_language)
        .map_err(|error| QueryError::ParserLanguage {
            language,
            message: error.to_string(),
        })?;
    parser
        .parse(source, None)
        .ok_or_else(|| QueryError::ParseReturnedNoTree {
            path: path.to_path_buf(),
        })
}

fn canonicalize_callee(raw_callee: &str, aliases: &HashMap<String, String>) -> String {
    let normalized = normalize_callee(raw_callee);

    if let Some(canonical) = aliases.get(normalized.as_str()) {
        return canonical.clone();
    }

    if let Some((head, tail)) = normalized.split_once('.') {
        if let Some(canonical_head) = aliases.get(head) {
            return format!("{canonical_head}.{tail}");
        }
    }

    normalized
}

fn call_matches_target(
    raw_callee: &str,
    canonical_callee: &str,
    snippet: &str,
    target: &CallTarget,
) -> bool {
    let raw = normalize_callee(raw_callee);
    let canonical = normalize_callee(canonical_callee);
    let target_callee = normalize_callee(&target.callee);

    if let Some(method) = &target.require_http_method {
        if final_segment(&canonical) == "fetch" && !snippet_mentions_http_method(snippet, method) {
            return false;
        }
    }

    if target_callee.contains('.') {
        raw.eq_ignore_ascii_case(&target_callee) || canonical.eq_ignore_ascii_case(&target_callee)
    } else {
        final_segment(&raw).eq_ignore_ascii_case(&target_callee)
            || final_segment(&canonical).eq_ignore_ascii_case(&target_callee)
    }
}

fn snippet_mentions_http_method(snippet: &str, method: &str) -> bool {
    let snippet = snippet.to_lowercase();
    let method = method.to_lowercase();

    snippet.contains(&format!("\"{method}\""))
        || snippet.contains(&format!("'{method}'"))
        || snippet.contains(&format!("method: {method}"))
        || snippet.contains(&format!("method={method}"))
}

fn normalize_callee(callee: &str) -> String {
    callee
        .replace("?.", ".")
        .replace("::", ".")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .to_owned()
}

fn final_segment(callee: &str) -> &str {
    callee
        .rsplit('.')
        .next()
        .unwrap_or(callee)
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
}

fn candidate_from_symbol(
    symbol: &Symbol,
    kind: CandidateKind,
) -> Result<QueryCandidate, QueryError> {
    let source = fs::read(&symbol.file).map_err(|source| QueryError::ReadFile {
        path: symbol.file.clone(),
        source,
    })?;

    Ok(QueryCandidate {
        kind,
        label: symbol.name.clone(),
        file: symbol.file.clone(),
        line_range: symbol.line_range,
        byte_range: symbol.byte_range.clone(),
        snippet: source_slice(&source, symbol.byte_range.start, symbol.byte_range.end),
        score: None,
        context: symbol.enclosing_scope.clone(),
    })
}

fn candidate_from_call_site(call_site: CallSite) -> QueryCandidate {
    QueryCandidate {
        kind: CandidateKind::CallSite,
        label: call_site.canonical_callee,
        file: call_site.file,
        line_range: call_site.line_range,
        byte_range: call_site.byte_range,
        snippet: call_site.snippet,
        score: None,
        context: String::new(),
    }
}

fn symbols_by_file(symbols: &[Symbol]) -> BTreeMap<PathBuf, Vec<&Symbol>> {
    let mut grouped = BTreeMap::new();

    for symbol in symbols {
        grouped
            .entry(symbol.file.clone())
            .or_insert_with(Vec::new)
            .push(symbol);
    }

    grouped
}

fn innermost_symbol_containing<'a>(
    symbols: &'a [&'a Symbol],
    byte_offset: usize,
) -> Option<&'a Symbol> {
    symbols
        .iter()
        .copied()
        .filter(|symbol| {
            symbol.byte_range.start <= byte_offset && byte_offset <= symbol.byte_range.end
        })
        .min_by_key(|symbol| symbol.byte_range.end - symbol.byte_range.start)
}

fn call_targets_from_query(query: &str) -> Option<Vec<CallTarget>> {
    let lower = query.to_lowercase();
    let mut targets = Vec::new();

    for identifier in identifiers_in_query(query) {
        if identifier.contains('.') {
            targets.push(CallTarget {
                callee: identifier,
                require_http_method: None,
            });
        }
    }

    if lower.contains("post request") || lower.contains("post") {
        targets.push(CallTarget {
            callee: "post".to_owned(),
            require_http_method: None,
        });

        targets.push(CallTarget {
            callee: "fetch".to_owned(),
            require_http_method: Some("post".to_owned()),
        });
    }

    if lower.contains("fetch") {
        targets.push(CallTarget {
            callee: "fetch".to_owned(),
            require_http_method: None,
        });
    }

    if targets.is_empty() {
        None
    } else {
        dedupe_call_targets(&mut targets);
        Some(targets)
    }
}

fn dedupe_call_targets(targets: &mut Vec<CallTarget>) {
    let mut seen = HashSet::new();
    targets.retain(|target| {
        seen.insert((
            target.callee.to_lowercase(),
            target.require_http_method.clone(),
        ))
    });
}

fn identifier_after_phrase(query: &str, phrase: &str) -> Option<String> {
    let query_lower = query.to_lowercase();
    let start = query_lower.find(phrase)? + phrase.len();
    first_identifier(&query[start..])
}

fn identifier_in_where_defined_query(query: &str) -> Option<String> {
    let lower = query.to_lowercase();

    if let Some(is_defined_at) = lower.find(" is defined") {
        let before = query[..is_defined_at]
            .trim()
            .trim_start_matches("where")
            .trim();
        return identifiers_in_query(before).pop();
    }

    if let Some(after_where_is) = lower.strip_prefix("where is ") {
        let defined_at = after_where_is.find(" defined")?;
        return first_identifier(&query["where is ".len()..][..defined_at]);
    }

    None
}

fn first_identifier(text: &str) -> Option<String> {
    identifiers_in_query(text).into_iter().next()
}

fn identifiers_in_query(query: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut current = String::new();

    for character in query.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | ':') {
            current.push(character);
        } else {
            push_identifier(&mut identifiers, &mut current);
        }
    }
    push_identifier(&mut identifiers, &mut current);

    identifiers
        .into_iter()
        .filter(|identifier| !is_query_stop_word(identifier))
        .collect()
}

fn push_identifier(identifiers: &mut Vec<String>, current: &mut String) {
    let identifier = current
        .trim_matches('.')
        .trim_matches(':')
        .trim_matches('_')
        .to_owned();
    current.clear();

    if !identifier.is_empty()
        && identifier
            .chars()
            .any(|character| character.is_ascii_alphabetic())
    {
        identifiers.push(identifier);
    }
}

fn is_query_stop_word(identifier: &str) -> bool {
    matches!(
        identifier.to_lowercase().as_str(),
        "a" | "all"
            | "an"
            | "and"
            | "call"
            | "calls"
            | "defined"
            | "every"
            | "everywhere"
            | "find"
            | "function"
            | "in"
            | "is"
            | "made"
            | "named"
            | "of"
            | "request"
            | "requests"
            | "show"
            | "the"
            | "to"
            | "where"
    )
}

fn query_requests_all(query: &str) -> bool {
    identifiers_including_stop_words(query)
        .iter()
        .any(|word| matches!(word.as_str(), "all" | "every" | "everywhere"))
}

fn identifiers_including_stop_words(query: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut current = String::new();

    for character in query.chars() {
        if character.is_ascii_alphanumeric() {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            identifiers.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        identifiers.push(current);
    }

    identifiers
}

fn names_match(actual: &str, requested: &str) -> bool {
    actual == requested
        || actual.eq_ignore_ascii_case(requested)
        || final_segment(actual).eq_ignore_ascii_case(requested)
}

fn source_slice(source: &[u8], start: usize, end: usize) -> String {
    String::from_utf8_lossy(&source[start..end])
        .trim()
        .to_owned()
}

fn format_line_range(line_range: &LineRange) -> String {
    if line_range.start == line_range.end {
        line_range.start.to_string()
    } else {
        format!("{}-{}", line_range.start, line_range.end)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::rc::Rc;

    use tempfile::tempdir;

    use crate::cache::CacheStore;
    use crate::llm::{LlmError, LlmProvider, RenderedPrompt};

    use super::{
        find_in_path, is_bare_name_query, run_semantic, CandidateKind, ConfidenceLevel,
        RetrievalMode,
    };

    #[test]
    fn bare_name_query_gating_separates_names_from_descriptions() {
        assert!(is_bare_name_query("login"));
        assert!(is_bare_name_query("verify_token"));
        assert!(is_bare_name_query("the login"));
        assert!(!is_bare_name_query(
            "the function that verifies a login token before issuing a session"
        ));
    }

    #[test]
    fn finds_function_definition_by_name() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("auth.py"),
            "def login(user):\n    return user\n\n\ndef verify_token(token):\n    return token\n",
        )
        .unwrap();

        let results = find_in_path(directory.path(), "where verify_token is defined").unwrap();

        assert_eq!(results.retrieval(), RetrievalMode::Structural);
        assert_eq!(results.candidates().len(), 1);
        let candidate = &results.candidates()[0];
        assert_eq!(candidate.kind, CandidateKind::FunctionDefinition);
        assert_eq!(candidate.label, "verify_token");
        assert_eq!(candidate.line_range.start, 5);
        assert!(candidate.snippet.starts_with("def verify_token"));
    }

    #[test]
    fn finds_call_site_with_python_import_alias() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("client.py"),
            r#"
import requests as r

def send_event(payload):
    ignored = "r.post('/not-real')"
    # r.post('/also-not-real')
    return r.post("https://example.test/events", json=payload)
"#,
        )
        .unwrap();

        let results = find_in_path(directory.path(), "where the POST request is made").unwrap();

        assert_eq!(results.candidates().len(), 1);
        let candidate = &results.candidates()[0];
        assert_eq!(candidate.kind, CandidateKind::CallSite);
        assert_eq!(candidate.label, "requests.post");
        assert!(candidate.snippet.starts_with("r.post("));
    }

    #[test]
    fn everywhere_query_returns_full_call_set() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("client.py"),
            r#"
import requests

def first(payload):
    return requests.post("https://example.test/a", json=payload)

def second(payload):
    return requests.post("https://example.test/b", json=payload)
"#,
        )
        .unwrap();

        let results = find_in_path(directory.path(), "show me everywhere POST request").unwrap();

        assert!(results.all_requested());
        assert_eq!(results.candidates().len(), 2);
        assert!(results
            .candidates()
            .iter()
            .all(|candidate| candidate.label == "requests.post"));
    }

    #[test]
    fn finds_function_containing_call() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("api.js"),
            r#"
function loadUser() {
  return fetch("/users", { method: "POST" });
}
"#,
        )
        .unwrap();

        let results = find_in_path(directory.path(), "the function containing fetch").unwrap();

        assert_eq!(results.candidates().len(), 1);
        let candidate = &results.candidates()[0];
        assert_eq!(candidate.kind, CandidateKind::EnclosingFunction);
        assert_eq!(candidate.label, "loadUser");
        assert!(candidate.snippet.starts_with("function loadUser"));
    }

    #[test]
    fn ignores_fetch_without_matching_http_method_for_post_request_query() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("api.js"),
            r#"
function loadUser() {
  return fetch("/users", { method: "GET" });
}

function createUser() {
  return fetch("/users", { method: "POST" });
}
"#,
        )
        .unwrap();

        let results = find_in_path(directory.path(), "where the POST request is made").unwrap();

        assert_eq!(results.candidates().len(), 1);
        assert!(results.candidates()[0].snippet.contains("POST"));
    }

    #[test]
    fn finds_go_call_site_with_import_alias_through_grammar_engine() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("client.go"),
            r#"
package main

import h "net/http"

func sendEvent(payload string) {
    _, _ = h.Post("https://example.test/events", "text/plain", nil)
}
"#,
        )
        .unwrap();

        let results = find_in_path(directory.path(), "call to http.Post").unwrap();

        assert_eq!(results.candidates().len(), 1);
        let candidate = &results.candidates()[0];
        assert_eq!(candidate.kind, CandidateKind::CallSite);
        assert_eq!(candidate.label, "http.Post");
        assert!(candidate.snippet.contains("h.Post("));
    }

    #[test]
    fn repeated_query_uses_query_cache_for_same_source_state() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("auth.py"),
            "def verify_token(token):\n    return token\n",
        )
        .unwrap();

        let first = find_in_path(directory.path(), "where verify_token is defined").unwrap();
        let second = find_in_path(directory.path(), "where verify_token is defined").unwrap();
        let cache = CacheStore::open_for_root(directory.path()).unwrap();
        let stats = cache.stats().unwrap();

        assert_eq!(first, second);
        assert_eq!(stats.query_entries, 1);
        assert_eq!(stats.query_misses, 1);
        assert_eq!(stats.query_hits, 1);
    }

    #[test]
    fn find_records_last_results_for_follow_up_explain() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("auth.py"),
            "def verify_token(token):\n    return token\n",
        )
        .unwrap();

        let results = find_in_path(directory.path(), "where verify_token is defined").unwrap();
        let cache = CacheStore::open_for_root(directory.path()).unwrap();
        let stored = cache
            .last_find_results::<super::QueryResults>()
            .unwrap()
            .expect("last find results stored");

        assert_eq!(stored, results);
    }

    // ----- Semantic path (LLM ranking) -----

    fn write_auth_fixture(directory: &Path) {
        fs::write(
            directory.join("auth.py"),
            "def verify_token(token):\n    return token and token.count(\".\") == 2\n\n\ndef compute_sum(values):\n    return sum(values)\n\n\ndef render_banner(text):\n    return f\"== {text} ==\"\n",
        )
        .unwrap();
    }

    use std::path::Path;

    #[test]
    fn semantic_find_routes_fuzzy_query_through_llm_to_right_function() {
        let directory = tempdir().unwrap();
        write_auth_fixture(directory.path());
        let root = directory.path().canonicalize().unwrap();
        let cache = CacheStore::open_for_root(&root).unwrap();
        let mut ranker = MockRanker::selecting(&["verify_token"], "high");

        let results = run_semantic(&root, &cache, "the auth call", false, &mut ranker).unwrap();

        assert_eq!(results.retrieval(), RetrievalMode::Semantic);
        assert_eq!(results.confidence(), ConfidenceLevel::High);
        assert_eq!(results.candidates().len(), 1);
        let candidate = &results.candidates()[0];
        assert_eq!(candidate.kind, CandidateKind::SemanticMatch);
        assert_eq!(candidate.label, "verify_token");
        assert!(candidate.score.unwrap() > 0.0);
        // The match shares no literal text with "auth".
        assert!(!candidate.snippet.to_lowercase().contains("auth"));
    }

    #[test]
    fn ambiguous_semantic_query_shows_pick_list_not_a_rephrase_wall() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("auth.py"),
            "def login(user):\n    return user\n\n\ndef verify_token(token):\n    return token\n\n\ndef check_password(pw):\n    return bool(pw)\n",
        )
        .unwrap();
        let root = directory.path().canonicalize().unwrap();
        let cache = CacheStore::open_for_root(&root).unwrap();
        let mut ranker =
            MockRanker::selecting(&["login", "verify_token", "check_password"], "medium");

        let results = run_semantic(&root, &cache, "auth", false, &mut ranker).unwrap();

        assert_eq!(results.confidence(), ConfidenceLevel::Medium);
        assert!(results.candidates().len() > 1);
        assert!(results.suggestions().is_empty());
    }

    #[test]
    fn low_confidence_semantic_query_offers_specific_suggestions() {
        let directory = tempdir().unwrap();
        write_auth_fixture(directory.path());
        let root = directory.path().canonicalize().unwrap();
        let cache = CacheStore::open_for_root(&root).unwrap();
        let mut ranker = MockRanker::no_match(&["did you mean billing, checkout, or refunds?"]);

        let results = run_semantic(&root, &cache, "the payment flow", false, &mut ranker).unwrap();

        assert_eq!(results.confidence(), ConfidenceLevel::Low);
        assert!(results.candidates().is_empty());
        assert_eq!(results.suggestions().len(), 1);
    }

    #[test]
    fn audit_semantic_query_returns_all_matches_with_scores() {
        let directory = tempdir().unwrap();
        write_auth_fixture(directory.path());
        let root = directory.path().canonicalize().unwrap();
        let cache = CacheStore::open_for_root(&root).unwrap();
        let mut ranker = MockRanker::selecting_all("medium");

        let results = run_semantic(&root, &cache, "every function", true, &mut ranker).unwrap();

        assert!(results.all_requested());
        assert_eq!(results.candidates().len(), 3);
        assert!(results
            .candidates()
            .iter()
            .all(|candidate| candidate.score.is_some()));
    }

    #[test]
    fn semantic_ranking_response_is_cached_per_prompt_and_model() {
        let directory = tempdir().unwrap();
        write_auth_fixture(directory.path());
        let root = directory.path().canonicalize().unwrap();
        let cache = CacheStore::open_for_root(&root).unwrap();
        let calls = Rc::new(Cell::new(0));
        let mut ranker = MockRanker::counting(&["verify_token"], "high", Rc::clone(&calls));

        let first = run_semantic(&root, &cache, "the auth call", false, &mut ranker).unwrap();
        let second = run_semantic(&root, &cache, "the auth call", false, &mut ranker).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            calls.get(),
            1,
            "second identical ranking should hit the cache"
        );
    }

    /// Deterministic stand-in for the LLM. It parses the real candidate list out of the
    /// prompt and selects by name, so the test exercises candidate gathering, prompt
    /// shape, JSON parsing, and routing without a network call.
    struct MockRanker {
        targets: Vec<String>,
        select_all: bool,
        confidence: String,
        suggestions: Vec<String>,
        calls: Option<Rc<Cell<usize>>>,
    }

    impl MockRanker {
        fn selecting(targets: &[&str], confidence: &str) -> Self {
            Self {
                targets: targets.iter().map(|t| (*t).to_owned()).collect(),
                select_all: false,
                confidence: confidence.to_owned(),
                suggestions: Vec::new(),
                calls: None,
            }
        }

        fn selecting_all(confidence: &str) -> Self {
            Self {
                targets: Vec::new(),
                select_all: true,
                confidence: confidence.to_owned(),
                suggestions: Vec::new(),
                calls: None,
            }
        }

        fn no_match(suggestions: &[&str]) -> Self {
            Self {
                targets: Vec::new(),
                select_all: false,
                confidence: "low".to_owned(),
                suggestions: suggestions.iter().map(|s| (*s).to_owned()).collect(),
                calls: None,
            }
        }

        fn counting(targets: &[&str], confidence: &str, calls: Rc<Cell<usize>>) -> Self {
            let mut ranker = Self::selecting(targets, confidence);
            ranker.calls = Some(calls);
            ranker
        }

        fn build_response(&self, prompt: &RenderedPrompt) -> String {
            let mut matches = Vec::new();
            for line in prompt.user.lines() {
                let Some((index, name)) = parse_candidate_line(line) else {
                    continue;
                };
                let selected = self.select_all || self.targets.iter().any(|t| t == &name);
                if selected {
                    matches.push(format!("{{\"index\":{index},\"score\":0.9}}"));
                }
            }
            let suggestions = self
                .suggestions
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"confidence\":\"{}\",\"matches\":[{}],\"suggestions\":[{}]}}",
                self.confidence,
                matches.join(","),
                suggestions
            )
        }
    }

    fn parse_candidate_line(line: &str) -> Option<(usize, String)> {
        let trimmed = line.trim_start();
        let (number, rest) = trimmed.split_once(". ")?;
        let index: usize = number.parse().ok()?;
        let name_field = rest.strip_prefix("name=")?;
        let name = name_field.split_whitespace().next()?.to_owned();
        Some((index, name))
    }

    impl LlmProvider for MockRanker {
        fn provider_name(&self) -> &str {
            "mock"
        }

        fn model(&self) -> &str {
            "mock-rank"
        }

        fn complete(&mut self, prompt: &RenderedPrompt) -> Result<String, LlmError> {
            if let Some(calls) = &self.calls {
                calls.set(calls.get() + 1);
            }
            Ok(self.build_response(prompt))
        }

        fn stream_explanation(
            &mut self,
            _prompt: &RenderedPrompt,
            _sink: &mut dyn FnMut(&str) -> Result<(), LlmError>,
        ) -> Result<(), LlmError> {
            Ok(())
        }
    }
}
