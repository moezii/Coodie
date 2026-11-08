use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use thiserror::Error;

use crate::audit::{audit_repository, testing, AuditError, AuditReport, SupportLevel, VerdictStatus};
use crate::cache::{CacheError, CacheStats, CacheStore};
use crate::index::{grammar_for, index_path, last_editor, watch_path, IndexError, IndexSummary};
use crate::llm::{explain_last_find_result, LlmError};
use crate::query::{
    find_in_path_with_llm, CandidateKind, ConfidenceLevel, QueryCandidate, QueryError,
    QueryResults, RetrievalMode,
};

/// Target width for right-aligned location columns in the styled UI.
const UI_WIDTH: usize = 78;

#[derive(Debug, Parser)]
#[command(
    name = "codereader",
    version,
    about = "Navigate source code by showing exact snippets for humans to read."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
    /// When to colorize output: auto (default), always, or never.
    #[arg(long, value_enum, global = true)]
    color: Option<ColorChoice>,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse supported source files and print an AST index summary.
    Index {
        /// Repository or directory to index.
        path: PathBuf,
    },
    /// Find code snippets: exact AST retrieval for structural queries, LLM ranking for
    /// conceptual ones (the LLM is in the loop the moment judgement is needed).
    Find {
        /// Natural-language or structural query, e.g. "the function that verifies a login token".
        query: String,
        /// Repository or directory to search.
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Override the LLM for this run, same shape as CODIE_LLM (provider,model,key,base_url).
        #[arg(long)]
        llm: Option<String>,
        /// Show git provenance (last editor/date/commit) for each match.
        #[arg(long)]
        blame: bool,
        /// Show an advisory, off-by-default heuristic guess that a match may be AI-generated.
        #[arg(long)]
        flag_ai: bool,
    },
    /// Explain one candidate from the previous find command with the opt-in LLM.
    Explain {
        /// One-based candidate number from the previous find output.
        result: usize,
        /// Repository or directory whose previous find result should be used.
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Override the LLM for this run, same shape as CODIE_LLM (provider,model,key,base_url).
        #[arg(long)]
        llm: Option<String>,
    },
    /// Inspect or clear the persistent SQLite cache.
    Cache {
        /// Print cache entry counts and hit/miss counters.
        #[arg(long)]
        stats: bool,
        /// Remove all persisted cache entries and counters.
        #[arg(long)]
        clear: bool,
        /// Repository or directory whose cache should be used.
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
    },
    /// Keep the index warm by watching source files for live edits.
    Watch {
        /// Repository or directory to watch.
        path: PathBuf,
    },
    /// Audit a repository: map its entrypoints, trace reachability, and report
    /// the code nobody can account for. Fully deterministic — no LLM involved.
    Audit {
        /// Repository or directory to audit.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit the full machine-readable report instead of the terminal summary.
        #[arg(long)]
        json: bool,
        /// Emit a client-facing Markdown report.
        #[arg(long)]
        report: bool,
        /// Add plain-English descriptions and check the README against the code.
        /// This is the only part of an audit that uses a model.
        #[arg(long)]
        describe: bool,
        /// Override the LLM for this run, same shape as CODIE_LLM.
        #[arg(long)]
        llm: Option<String>,
        /// Maximum unaccounted findings to list (0 for all).
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Exit non-zero if the score is below this value. For CI gating.
        #[arg(long, value_name = "SCORE")]
        fail_under: Option<f64>,
        /// Exit non-zero if any externally reachable entrypoint has no test.
        #[arg(long)]
        fail_on_untested: bool,
        /// Compare against a previously saved `--json` report and exit non-zero
        /// if the score dropped or new untested entrypoints appeared.
        #[arg(long, value_name = "FILE")]
        baseline: Option<PathBuf>,
    },
}

#[derive(Debug, Error)]
pub enum CliError {
    /// A quality gate tripped. The audit itself succeeded; the repository did
    /// not meet the bar the caller asked for.
    #[error("audit gate failed: {0}")]
    Gate(String),
    #[error("cannot read baseline {path}: {source}")]
    Baseline {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("baseline {path} is not a saved audit report: {source}")]
    BaselineParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error("failed to canonicalize cache path {path}: {source}")]
    CachePath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write CLI output: {0}")]
    Output(#[from] io::Error),
}

pub fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    let style = Style::resolve(cli.color.unwrap_or_default(), io::stdout().is_terminal());
    execute_styled(cli, &mut io::stdout(), &style)
}

/// Plain-text execution used by tests and any non-interactive caller.
pub fn execute<W: Write>(cli: Cli, writer: &mut W) -> Result<(), CliError> {
    execute_styled(cli, writer, &Style::plain())
}

fn execute_styled<W: Write>(cli: Cli, writer: &mut W, style: &Style) -> Result<(), CliError> {
    match cli.command {
        Command::Index { path } => {
            let summary = index_path(path)?;
            print_index_summary(&summary, writer, style)?;
        }
        Command::Find {
            query,
            path,
            llm,
            blame,
            flag_ai,
        } => {
            let results = find_in_path_with_llm(path, &query, llm.as_deref())?;
            let options = RenderOptions { blame, flag_ai };
            print_query_results(&results, writer, style, &options)?;
        }
        Command::Explain { result, path, llm } => {
            let root = canonicalize_cache_root(path)?;
            explain_last_find_result(&root, result, llm.as_deref(), writer)?;
        }
        Command::Cache { stats, clear, path } => {
            let root = canonicalize_cache_root(path)?;
            let cache = CacheStore::open_for_root(&root)?;
            if clear {
                cache.clear()?;
                writeln!(writer, "Cache cleared: {}", cache.path().display())?;
            }
            if stats || !clear {
                let stats = cache.stats()?;
                print_cache_stats(&stats, writer, style)?;
            }
        }
        Command::Watch { path } => {
            watch_path(path, writer)?;
        }
        Command::Audit {
            path,
            json,
            report: as_markdown,
            describe,
            llm,
            limit,
            fail_under,
            fail_on_untested,
            baseline,
        } => {
            let mut audit = audit_repository(&path)?;
            if describe {
                let root = canonicalize_cache_root(path.clone())?;
                let mut provider = crate::llm::provider_from_env(&root, llm.as_deref())?;
                let cache = CacheStore::open_for_root(&root).ok();
                crate::audit::describe_report(&mut audit, provider.as_mut(), cache.as_ref())?;
            }
            let audit = audit;
            if json {
                writeln!(writer, "{}", audit.to_json()?)?;
            } else if as_markdown {
                write!(writer, "{}", crate::audit::report::markdown(&audit))?;
            } else {
                print_audit_report(&audit, limit, writer, style)?;
            }

            if let Some(reason) = evaluate_gates(&audit, fail_under, fail_on_untested, baseline)? {
                return Err(CliError::Gate(reason));
            }
        }
    }

    Ok(())
}

/// Decide whether this audit should fail a build.
///
/// Returns `Some(reason)` when a gate tripped. The report is always printed
/// first — a CI failure that does not say what regressed is a failure people
/// learn to rerun rather than read.
fn evaluate_gates(
    audit: &AuditReport,
    fail_under: Option<f64>,
    fail_on_untested: bool,
    baseline: Option<PathBuf>,
) -> Result<Option<String>, CliError> {
    if let Some(threshold) = fail_under {
        if audit.score.score < threshold {
            return Ok(Some(format!(
                "score {:.1} is below the required {:.1}",
                audit.score.score, threshold
            )));
        }
    }

    if fail_on_untested && !audit.tests.untested_entrypoints.is_empty() {
        let count = audit.tests.untested_entrypoints.len();
        let first = audit
            .tests
            .untested_entrypoints
            .iter()
            .take(3)
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(Some(format!(
            "{count} entrypoint(s) have no test, including: {first}"
        )));
    }

    if let Some(path) = baseline {
        let previous = std::fs::read_to_string(&path).map_err(|source| CliError::Baseline {
            path: path.clone(),
            source,
        })?;
        let previous: AuditReport =
            serde_json::from_str(&previous).map_err(|source| CliError::BaselineParse {
                path: path.clone(),
                source,
            })?;

        // A tenth of a point is noise from a comment change; a real regression
        // is larger than that and this avoids failing builds over rounding.
        if audit.score.score + 0.1 < previous.score.score {
            return Ok(Some(format!(
                "score dropped from {:.1} to {:.1}",
                previous.score.score, audit.score.score
            )));
        }

        // Keyed by file and name, not name alone. A repository can hold four
        // different functions called `label`, and matching on the bare name
        // both invents gaps that are not new and hides real ones behind an
        // unrelated function that happens to share a name.
        let identify = |entry: &testing::UntestedEntrypoint| {
            format!("{}:{}", entry.file, entry.name)
        };
        let known: std::collections::HashSet<String> = previous
            .tests
            .untested_entrypoints
            .iter()
            .map(&identify)
            .collect();
        let new_gaps: Vec<String> = audit
            .tests
            .untested_entrypoints
            .iter()
            .filter(|entry| !known.contains(&identify(entry)))
            .map(|entry| format!("{} ({})", entry.name, entry.file))
            .collect();
        if !new_gaps.is_empty() {
            // Name only the first few. A large merge can add dozens, and a
            // failure message nobody can read is a failure nobody acts on.
            let shown = new_gaps
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let rest = new_gaps.len().saturating_sub(5);
            let tail = if rest > 0 {
                format!(", and {rest} more")
            } else {
                String::new()
            };
            return Ok(Some(format!(
                "{} public symbol(s) newly have no test: {shown}{tail}",
                new_gaps.len()
            )));
        }
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Styling
// ---------------------------------------------------------------------------

/// A tiny ANSI styler. When `color` is false every helper returns the text
/// unchanged, so piped/CI output is plain and byte-stable.
struct Style {
    color: bool,
}

impl Style {
    fn plain() -> Self {
        Self { color: false }
    }

    fn resolve(choice: ColorChoice, is_tty: bool) -> Self {
        let color = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => is_tty && std::env::var_os("NO_COLOR").is_none(),
        };
        Self { color }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    fn cyan(&self, text: &str) -> String {
        self.paint("36", text)
    }

    fn accent(&self, text: &str) -> String {
        self.paint("1;35", text)
    }
}

// ---------------------------------------------------------------------------
// Audit report
// ---------------------------------------------------------------------------

/// Render the audit as something a human reads top to bottom.
///
/// Findings and limitations are printed under separate headings on purpose. One
/// is about the audited code; the other is about how much this engine could see.
/// Collapsing them is what made the predecessor report healthy repositories as
/// failures.
fn print_audit_report<W: Write>(
    report: &AuditReport,
    limit: usize,
    writer: &mut W,
    style: &Style,
) -> Result<(), CliError> {
    let coverage = &report.coverage;
    let score = &report.score;
    let verdict = &report.verdict;

    writeln!(writer, "{}", style.bold(&report.root.display().to_string()))?;
    writeln!(
        writer,
        "{}",
        style.dim(&format!(
            "{} files - {} symbols - {} entrypoints ({} external)",
            report.files,
            report.symbols,
            coverage.entrypoint_count,
            coverage.external_entrypoint_count
        ))
    )?;
    writeln!(writer)?;

    let (verdict_text, verdict_code) = match verdict.status {
        VerdictStatus::Clean => ("CLEAN", "1;32"),
        VerdictStatus::FindingsFound => ("FINDINGS", "1;33"),
        VerdictStatus::Inconclusive => ("INCONCLUSIVE", "1;36"),
    };
    writeln!(
        writer,
        "{}  score {} ({})   confidence {:.0}%",
        style.paint(verdict_code, verdict_text),
        style.bold(&format!("{:.1}", score.score)),
        score.grade,
        verdict.confidence * 100.0
    )?;
    writeln!(writer)?;

    writeln!(writer, "{}", style.bold("Coverage"))?;
    writeln!(
        writer,
        "  {:.1}% of {} meaningful nodes accounted for ({} unaccounted)",
        coverage.coverage_percent, coverage.meaningful_nodes, coverage.unaccounted_nodes
    )?;
    let counts = &coverage.counts;
    writeln!(
        writer,
        "  {}",
        style.dim(&format!(
            "feature {} - infrastructure {} - utility {} - test {} - config {} - docs {} - generated {} - dead {} - unknown {}",
            counts.feature,
            counts.infrastructure,
            counts.utility,
            counts.test,
            counts.configuration,
            counts.documentation,
            counts.generated,
            counts.dead,
            counts.unknown
        ))
    )?;
    writeln!(writer)?;

    writeln!(writer, "{}", style.bold("Subscores"))?;
    writeln!(
        writer,
        "  coverage {:.2}   clarity {:.2}   structure {:.2}   tests {:.2}",
        score.coverage, score.clarity, score.structure, score.tests
    )?;
    writeln!(writer)?;

    if !verdict.findings.is_empty() {
        writeln!(writer, "{}", style.bold("Findings (about this code)"))?;
        for finding in &verdict.findings {
            writeln!(writer, "  - {finding}")?;
        }
        writeln!(writer)?;
    }

    if !verdict.limitations.is_empty() {
        // Not "limitations": this is what could not be proven either way, and
        // naming it that way stops it reading as a defect in the audited code.
        writeln!(writer, "{}", style.bold("Not proven either way"))?;
        for limitation in &verdict.limitations {
            writeln!(writer, "  - {limitation}")?;
        }
        writeln!(writer)?;
    }

    if !coverage.public_unused.is_empty() {
        writeln!(
            writer,
            "{}",
            style.bold("Public, but nothing here uses it")
        )?;
        writeln!(
            writer,
            "  {}",
            style.dim("Expected if consumers call these. Dead code if this is an application.")
        )?;
        let shown = if limit == 0 {
            coverage.public_unused.len()
        } else {
            limit.min(coverage.public_unused.len())
        };
        for finding in coverage.public_unused.iter().take(shown) {
            writeln!(
                writer,
                "  {} {}",
                style.cyan(&finding.name),
                style.dim(&format!(
                    "{}:{}",
                    finding.file, finding.line_range.start
                ))
            )?;
        }
        if shown < coverage.public_unused.len() {
            writeln!(
                writer,
                "  {}",
                style.dim(&format!(
                    "... and {} more",
                    coverage.public_unused.len() - shown
                ))
            )?;
        }
        writeln!(writer)?;
    }

    if !coverage.unaccounted.is_empty() {
        writeln!(writer, "{}", style.bold("Unaccounted code"))?;
        let shown = if limit == 0 {
            coverage.unaccounted.len()
        } else {
            limit.min(coverage.unaccounted.len())
        };
        for finding in coverage.unaccounted.iter().take(shown) {
            writeln!(
                writer,
                "  {} {}",
                style.cyan(&finding.name),
                style.dim(&format!(
                    "{}:{}-{}  [{}{}]",
                    finding.file,
                    finding.line_range.start,
                    finding.line_range.end,
                    finding.classification.label(),
                    match (&finding.reachability, &finding.mentioned_at) {
                        (crate::audit::Reachability::MentionedNotTraced, Some(site)) => {
                            format!(", mentioned but not traced: {site}")
                        }
                        (crate::audit::Reachability::MentionedNotTraced, None) => {
                            ", mentioned but not traced".to_owned()
                        }
                        _ => String::new(),
                    }
                ))
            )?;
        }
        if shown < coverage.unaccounted.len() {
            writeln!(
                writer,
                "  {}",
                style.dim(&format!(
                    "... and {} more (use --limit 0 to list all)",
                    coverage.unaccounted.len() - shown
                ))
            )?;
        }
        writeln!(writer)?;
    }

    let tests = &report.tests;
    writeln!(writer, "{}", style.bold("Test coverage of the public surface"))?;
    writeln!(
        writer,
        "  {} of {} entrypoints and exported functions are exercised by a test ({:.0}%)",
        tests.tested_entrypoints,
        tests.external_entrypoints,
        tests.entrypoint_test_coverage * 100.0
    )?;
    writeln!(
        writer,
        "  {}",
        style.dim(&format!(
            "{} test function(s); {} assert nothing",
            tests.test_functions,
            tests.assertion_free_tests.len()
        ))
    )?;
    if !tests.untested_entrypoints.is_empty() {
        let shown = if limit == 0 {
            tests.untested_entrypoints.len()
        } else {
            limit.min(tests.untested_entrypoints.len())
        };
        writeln!(writer, "  {}", style.bold("Untested public surface"))?;
        for entrypoint in tests.untested_entrypoints.iter().take(shown) {
            writeln!(
                writer,
                "    {} {}",
                style.cyan(&entrypoint.name),
                style.dim(&format!(
                    "{}:{}",
                    entrypoint.file, entrypoint.line_range.start
                ))
            )?;
        }
        if shown < tests.untested_entrypoints.len() {
            writeln!(
                writer,
                "    {}",
                style.dim(&format!(
                    "... and {} more",
                    tests.untested_entrypoints.len() - shown
                ))
            )?;
        }
    }
    if !tests.assertion_free_tests.is_empty() {
        writeln!(
            writer,
            "  {}",
            style.bold("Tests that assert nothing (cannot fail)")
        )?;
        for test in tests.assertion_free_tests.iter().take(10) {
            writeln!(
                writer,
                "    {} {}",
                style.cyan(&test.name),
                style.dim(&format!("{}:{}", test.file, test.line_range.start))
            )?;
        }
    }
    writeln!(writer)?;

    if !score.suggestions.is_empty() {
        writeln!(writer, "{}", style.bold("How to raise the score"))?;
        for suggestion in &score.suggestions {
            writeln!(writer, "  - {suggestion}")?;
        }
        writeln!(writer)?;
    }

    writeln!(writer, "{}", style.bold("Language support"))?;
    for (language, support) in &report.languages {
        let level = match support.support {
            SupportLevel::Entrypoints => "entrypoints",
            SupportLevel::Baseline => "baseline",
        };
        writeln!(
            writer,
            "  {:<14} {:>4} file(s) {:>5} symbol(s)  {}",
            language,
            support.files,
            support.symbols,
            style.dim(level)
        )?;
    }
    if !report.vendored.is_empty() {
        writeln!(
            writer,
            "  {}",
            style.dim(&format!(
                "vendored, not audited: {}",
                report.vendored.join(", ")
            ))
        )?;
    }

    Ok(())
}

fn confidence_badge(style: &Style, results: &QueryResults) -> String {
    let (text, code) = match (results.retrieval(), results.confidence()) {
        (RetrievalMode::Structural, _) => ("exact match", "1;32"),
        (RetrievalMode::Semantic, ConfidenceLevel::High) => ("strong match", "1;32"),
        (RetrievalMode::Semantic, ConfidenceLevel::Medium) => ("several matches", "1;33"),
        (RetrievalMode::Semantic, ConfidenceLevel::Low) => ("no strong match", "1;31"),
    };
    style.paint(code, text)
}

// ---------------------------------------------------------------------------
// Find results
// ---------------------------------------------------------------------------

/// Opt-in, advisory annotations layered onto each match.
struct RenderOptions {
    blame: bool,
    flag_ai: bool,
}

fn print_query_results<W: Write>(
    results: &QueryResults,
    writer: &mut W,
    style: &Style,
    options: &RenderOptions,
) -> Result<(), io::Error> {
    if style.color {
        print_query_results_styled(results, writer, style, options)
    } else {
        print_query_results_plain(results, writer, style, options)
    }
}

fn print_query_results_styled<W: Write>(
    results: &QueryResults,
    writer: &mut W,
    style: &Style,
    options: &RenderOptions,
) -> Result<(), io::Error> {
    let query = results.query();

    if matches!(results.confidence(), ConfidenceLevel::Low) && !results.suggestions().is_empty() {
        writeln!(
            writer,
            "{}   {}",
            style.bold(query),
            confidence_badge(style, results)
        )?;
        writeln!(writer, "{}", style.dim("did you mean"))?;
        for suggestion in results.suggestions() {
            writeln!(writer, "  {} {}", style.accent("->"), suggestion)?;
        }
        if results.candidates().is_empty() {
            return Ok(());
        }
        writeln!(writer)?;
    }

    if results.candidates().is_empty() {
        writeln!(
            writer,
            "{}",
            style.dim(&format!("no matches for \"{query}\""))
        )?;
        return Ok(());
    }

    let count = results.candidates().len();
    writeln!(
        writer,
        "{}   {}",
        style.bold(query),
        confidence_badge(style, results)
    )?;
    let rule = format!("-- {count} result{} ", plural(count));
    writeln!(writer, "{}", style.dim(&pad_rule(&rule, UI_WIDTH)))?;
    writeln!(writer)?;

    for (index, candidate) in results.candidates().iter().enumerate() {
        render_card(writer, style, results, index, candidate, options)?;
        writeln!(writer)?;
    }

    if matches!(results.retrieval(), RetrievalMode::Semantic) {
        writeln!(
            writer,
            "{}",
            style.dim("read the code, then `explain <n>` if you want the LLM's take (opt-in)")
        )?;
    }

    Ok(())
}

fn render_card<W: Write>(
    writer: &mut W,
    style: &Style,
    results: &QueryResults,
    index: usize,
    candidate: &QueryCandidate,
    options: &RenderOptions,
) -> Result<(), io::Error> {
    let number = index + 1;
    let relpath = candidate_display_path(results, candidate)
        .display()
        .to_string();
    let lines = format_line_range(&candidate.line_range);

    let left_raw = format!(
        "{number}  {}  {}",
        candidate.label,
        kind_tag(candidate.kind)
    );
    let left = format!(
        "{}  {}  {}",
        style.accent(&number.to_string()),
        style.bold(&candidate.label),
        style.dim(&kind_tag(candidate.kind))
    );

    let mut right_raw = format!("{relpath}:{lines}");
    let mut right = format!(
        "{}{}",
        style.cyan(&relpath),
        style.dim(&format!(":{lines}"))
    );
    if let Some(score) = candidate.score {
        let suffix = format!("  {score:.2}");
        right_raw.push_str(&suffix);
        right.push_str(&style.dim(&suffix));
    }

    writeln!(
        writer,
        "{}",
        align_row(&left, &left_raw, &right, &right_raw, UI_WIDTH)
    )?;

    if !candidate.context.is_empty() && candidate.context != "module" {
        writeln!(
            writer,
            "   {}",
            style.dim(&format!("in {}", candidate.context))
        )?;
    }

    annotate(writer, style, results, candidate, options)?;

    let start = candidate.line_range.start;
    for (offset, line) in candidate.snippet.lines().enumerate() {
        let gutter = style.dim(&format!("{:>4} | ", start + offset));
        let body = if is_comment_line(line) {
            style.dim(line)
        } else {
            line.to_owned()
        };
        writeln!(writer, "   {gutter}{body}")?;
    }

    Ok(())
}

fn print_query_results_plain<W: Write>(
    results: &QueryResults,
    writer: &mut W,
    style: &Style,
    options: &RenderOptions,
) -> Result<(), io::Error> {
    if results.candidates().is_empty() && results.suggestions().is_empty() {
        writeln!(writer, "No matches found for: {}", results.query())?;
        return Ok(());
    }

    match results.confidence() {
        ConfidenceLevel::Low if !results.suggestions().is_empty() => {
            writeln!(
                writer,
                "No strong match for \"{}\". Did you mean:",
                results.query()
            )?;
            for suggestion in results.suggestions() {
                writeln!(writer, "  - {suggestion}")?;
            }
            if results.candidates().is_empty() {
                return Ok(());
            }
            writeln!(writer)?;
        }
        ConfidenceLevel::Medium => {
            writeln!(
                writer,
                "Several matches for \"{}\" - pick the one you meant:",
                results.query()
            )?;
        }
        _ => {}
    }

    writeln!(writer, "Found {} candidate(s)", results.candidates().len())?;

    for (index, candidate) in results.candidates().iter().enumerate() {
        let display_path = candidate_display_path(results, candidate);
        let mut header = format!(
            "{}. {}:{} [{}] {}",
            index + 1,
            display_path.display(),
            format_line_range(&candidate.line_range),
            candidate.kind,
            candidate.label
        );
        if let Some(score) = candidate.score {
            header.push_str(&format!("  (score {score:.2})"));
        }
        if !candidate.context.is_empty() && candidate.context != "module" {
            header.push_str(&format!("  - in {}", candidate.context));
        }
        writeln!(writer, "{header}")?;
        annotate(writer, style, results, candidate, options)?;
        writeln!(writer, "{}", candidate.snippet)?;
        if index + 1 < results.candidates().len() {
            writeln!(writer)?;
        }
    }

    Ok(())
}

/// Emit opt-in provenance and the advisory AI flag for a single match.
fn annotate<W: Write>(
    writer: &mut W,
    style: &Style,
    results: &QueryResults,
    candidate: &QueryCandidate,
    options: &RenderOptions,
) -> Result<(), io::Error> {
    if options.flag_ai {
        if let Some(reason) = ai_advisory(&candidate.snippet) {
            writeln!(
                writer,
                "   {}",
                style.paint(
                    "33",
                    &format!(
                        "advisory: possibly AI-generated - {reason} (heuristic, not a verdict)"
                    )
                )
            )?;
        }
    }

    if options.blame {
        let provenance = last_editor(results.root(), &candidate.file, &candidate.line_range)
            .ok()
            .flatten();
        let line = match provenance {
            Some(p) => format!(
                "last edit: {} on {} ({}) \"{}\"",
                p.author, p.date, p.short_sha, p.summary
            ),
            None => "last edit: local / uncommitted".to_owned(),
        };
        writeln!(writer, "   {}", style.dim(&line))?;
    }

    Ok(())
}

/// Conservative, advisory-only signal. It does NOT claim to detect AI authorship; it
/// flags explicit generation markers or an unusually high comment ratio, and the output
/// always states it is a heuristic, not a verdict. Off by default.
fn ai_advisory(snippet: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "generated by",
        "auto-generated",
        "autogenerated",
        "ai-generated",
        "ai generated",
        "as an ai",
        "language model",
        "chatgpt",
        "copilot",
        "this code was generated",
    ];

    let lower = snippet.to_lowercase();
    if let Some(marker) = MARKERS.iter().find(|marker| lower.contains(**marker)) {
        return Some(format!("contains the marker \"{marker}\""));
    }

    let lines: Vec<&str> = snippet
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() >= 6 {
        let comments = lines.iter().filter(|line| is_comment_line(line)).count();
        if comments * 2 > lines.len() {
            return Some("unusually high comment-to-code ratio".to_owned());
        }
    }

    None
}

fn kind_tag(kind: CandidateKind) -> String {
    format!("[{kind}]")
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn pad_rule(prefix: &str, width: usize) -> String {
    let used = prefix.chars().count();
    let dashes = width.saturating_sub(used);
    format!("{prefix}{}", "-".repeat(dashes))
}

fn align_row(left: &str, left_raw: &str, right: &str, right_raw: &str, width: usize) -> String {
    let used = left_raw.chars().count() + right_raw.chars().count();
    let pad = width.saturating_sub(used).max(2);
    format!("{left}{}{right}", " ".repeat(pad))
}

fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#')
        || trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("--")
}

// ---------------------------------------------------------------------------
// Index + cache summaries (plain key/value; unchanged layout)
// ---------------------------------------------------------------------------

fn print_index_summary<W: Write>(
    summary: &IndexSummary,
    writer: &mut W,
    style: &Style,
) -> Result<(), io::Error> {
    writeln!(
        writer,
        "Indexed {}",
        style.cyan(&summary.root().display().to_string())
    )?;
    writeln!(writer, "Files parsed: {}", summary.files_parsed())?;
    writeln!(writer, "Files reparsed: {}", summary.files_reparsed())?;
    writeln!(
        writer,
        "Git changed source files: {}",
        summary.changed_source_files()
    )?;
    writeln!(
        writer,
        "Dirty source files: {}",
        summary.dirty_source_files()
    )?;
    writeln!(writer, "AST cache hits: {}", summary.ast_cache_hits())?;
    writeln!(writer, "AST cache misses: {}", summary.ast_cache_misses())?;
    writeln!(
        writer,
        "Functions found: {}",
        style.bold(&summary.functions_found().to_string())
    )?;

    if summary.language_stats().is_empty() {
        writeln!(writer, "Languages detected: none")?;
        return Ok(());
    }

    let languages = summary
        .language_stats()
        .keys()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(writer, "Languages detected: {languages}")?;
    let grammar_backed = summary
        .language_stats()
        .keys()
        .filter(|language| grammar_for(language).is_some())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let detection_only = summary
        .language_stats()
        .keys()
        .filter(|language| grammar_for(language).is_none())
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    writeln!(
        writer,
        "Grammar-backed languages: {}",
        language_list_or_none(&grammar_backed)
    )?;
    writeln!(
        writer,
        "Languages without a grammar: {}",
        language_list_or_none(&detection_only)
    )?;

    for (language, stats) in summary.language_stats() {
        writeln!(
            writer,
            "  {language}: {} file(s), {} function(s)",
            stats.files_parsed, stats.functions_found
        )?;
    }

    Ok(())
}

fn language_list_or_none(languages: &[String]) -> String {
    if languages.is_empty() {
        "none".to_owned()
    } else {
        languages.join(", ")
    }
}

fn print_cache_stats<W: Write>(
    stats: &CacheStats,
    writer: &mut W,
    style: &Style,
) -> Result<(), io::Error> {
    writeln!(
        writer,
        "Cache: {}",
        style.cyan(&stats.path.display().to_string())
    )?;
    writeln!(writer, "AST entries: {}", stats.ast_entries)?;
    writeln!(writer, "Query entries: {}", stats.query_entries)?;
    writeln!(writer, "Embedding entries: {}", stats.embedding_entries)?;
    writeln!(
        writer,
        "LLM response entries: {}",
        stats.llm_response_entries
    )?;
    writeln!(
        writer,
        "AST hits/misses: {}/{}",
        stats.ast_hits, stats.ast_misses
    )?;
    writeln!(
        writer,
        "Query hits/misses: {}/{}",
        stats.query_hits, stats.query_misses
    )?;
    Ok(())
}

fn canonicalize_cache_root(path: PathBuf) -> Result<PathBuf, CliError> {
    path.canonicalize()
        .map_err(|source| CliError::CachePath { path, source })
}

fn format_line_range(line_range: &crate::index::LineRange) -> String {
    if line_range.start == line_range.end {
        line_range.start.to_string()
    } else {
        format!("{}-{}", line_range.start, line_range.end)
    }
}

fn candidate_display_path<'a>(
    results: &'a QueryResults,
    candidate: &'a QueryCandidate,
) -> &'a Path {
    if results.root().as_os_str().is_empty() {
        return &candidate.file;
    }

    candidate
        .file
        .strip_prefix(results.root())
        .unwrap_or(&candidate.file)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;
    use tempfile::tempdir;

    use super::{execute, Cli};

    #[test]
    fn index_command_prints_summary() {
        let directory = tempdir().expect("temp dir");
        fs::write(
            directory.path().join("sample.py"),
            "def authenticate(user):\n    return user is not None\n",
        )
        .expect("fixture write");

        let cli = Cli::parse_from(["codereader", "index", directory.path().to_str().unwrap()]);
        let mut output = Vec::new();

        execute(cli, &mut output).expect("index command succeeds");

        let output = String::from_utf8(output).expect("valid utf8");
        assert!(output.contains("Files parsed: 1"), "{output}");
        assert!(output.contains("Files reparsed: 1"), "{output}");
        assert!(output.contains("Functions found: 1"), "{output}");
        assert!(output.contains("Languages detected: Python"), "{output}");
        assert!(
            output.contains("Grammar-backed languages: Python"),
            "{output}"
        );
        assert!(
            output.contains("Languages without a grammar: none"),
            "{output}"
        );
    }

    #[test]
    fn find_command_prints_bare_snippet_candidates() {
        let directory = tempdir().expect("temp dir");
        fs::write(
            directory.path().join("sample.py"),
            "def authenticate(user):\n    return user is not None\n",
        )
        .expect("fixture write");

        let cli = Cli::parse_from([
            "codereader",
            "find",
            "where authenticate is defined",
            "--path",
            directory.path().to_str().unwrap(),
        ]);
        let mut output = Vec::new();

        execute(cli, &mut output).expect("find command succeeds");

        let output = String::from_utf8(output).expect("valid utf8");
        assert!(output.contains("Found 1 candidate(s)"), "{output}");
        assert!(output.contains("[function] authenticate"), "{output}");
        assert!(output.contains("def authenticate(user):"), "{output}");
        assert!(!output.contains("Explanation"), "{output}");
    }

    #[test]
    fn color_never_keeps_plain_output() {
        let directory = tempdir().expect("temp dir");
        fs::write(
            directory.path().join("sample.py"),
            "def authenticate(user):\n    return user is not None\n",
        )
        .expect("fixture write");

        let cli = Cli::parse_from([
            "codereader",
            "--color",
            "never",
            "find",
            "where authenticate is defined",
            "--path",
            directory.path().to_str().unwrap(),
        ]);
        let mut output = Vec::new();

        execute(cli, &mut output).expect("find command succeeds");

        let output = String::from_utf8(output).expect("valid utf8");
        assert!(
            !output.contains('\u{1b}'),
            "expected no ANSI escapes: {output:?}"
        );
        assert!(output.contains("Found 1 candidate(s)"), "{output}");
    }

    #[test]
    fn cache_command_prints_stats_and_can_clear() {
        let directory = tempdir().expect("temp dir");

        let cli = Cli::parse_from([
            "codereader",
            "cache",
            "--stats",
            "--path",
            directory.path().to_str().unwrap(),
        ]);
        let mut output = Vec::new();

        execute(cli, &mut output).expect("cache stats succeeds");

        let output = String::from_utf8(output).expect("valid utf8");
        assert!(output.contains("AST entries: 0"), "{output}");
        assert!(output.contains("Query entries: 0"), "{output}");

        let cli = Cli::parse_from([
            "codereader",
            "cache",
            "--clear",
            "--path",
            directory.path().to_str().unwrap(),
        ]);
        let mut output = Vec::new();

        execute(cli, &mut output).expect("cache clear succeeds");

        let output = String::from_utf8(output).expect("valid utf8");
        assert!(output.contains("Cache cleared:"), "{output}");
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use tempfile::tempdir;

    /// A small service with one route that no test reaches.
    fn sample_audit() -> AuditReport {
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        std::fs::create_dir_all(root.join("app")).expect("dirs");
        std::fs::write(
            root.join("app").join("main.py"),
            "from fastapi import FastAPI\n\napp = FastAPI()\n\n\
             @app.get(\"/users\")\ndef get_users():\n    return []\n",
        )
        .expect("write");
        audit_repository(root).expect("audit")
    }

    #[test]
    fn fail_under_trips_only_below_the_threshold() {
        let mut audit = sample_audit();
        audit.score.score = 70.0;

        assert!(evaluate_gates(&audit, Some(60.0), false, None)
            .expect("gate")
            .is_none());
        let tripped = evaluate_gates(&audit, Some(80.0), false, None).expect("gate");
        assert!(
            tripped.is_some_and(|reason| reason.contains("70.0") && reason.contains("80.0")),
            "the reason must name both the score and the bar"
        );
    }

    #[test]
    fn fail_on_untested_names_what_is_untested() {
        let audit = sample_audit();
        let reason = evaluate_gates(&audit, None, true, None).expect("gate");
        assert!(
            reason.is_some_and(|text| text.contains("no test")),
            "a route with no test should trip the gate"
        );
    }

    #[test]
    fn a_baseline_catches_a_dropped_score() {
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("baseline.json");

        let mut previous = sample_audit();
        previous.score.score = 90.0;
        std::fs::write(&path, previous.to_json().expect("json")).expect("write");

        let mut current = sample_audit();
        current.score.score = 80.0;

        let reason = evaluate_gates(&current, None, false, Some(path)).expect("gate");
        assert!(
            reason.is_some_and(|text| text.contains("90.0") && text.contains("80.0")),
            "a ten point drop must trip the gate"
        );
    }

    #[test]
    fn a_baseline_tolerates_rounding_noise() {
        // A comment change can move the score a hundredth of a point. Failing a
        // build on that teaches people to ignore the gate.
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("baseline.json");

        let mut previous = sample_audit();
        previous.score.score = 90.0;
        std::fs::write(&path, previous.to_json().expect("json")).expect("write");

        let mut current = sample_audit();
        current.score.score = 89.95;

        assert!(
            evaluate_gates(&current, None, false, Some(path))
                .expect("gate")
                .is_none(),
            "a 0.05 drop is noise, not a regression"
        );
    }

    #[test]
    fn a_baseline_ignores_gaps_that_already_existed() {
        // The point of a baseline is to stop the bleeding, not to fail every
        // build until an old backlog is cleared.
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("baseline.json");

        let previous = sample_audit();
        std::fs::write(&path, previous.to_json().expect("json")).expect("write");

        let current = sample_audit();
        assert!(
            evaluate_gates(&current, None, false, Some(path))
                .expect("gate")
                .is_none(),
            "an unchanged repository must pass its own baseline"
        );
    }

    #[test]
    fn two_functions_sharing_a_name_are_told_apart() {
        // A repository can hold four different `label` functions. Keying the
        // comparison on the bare name reported all of them as newly untested
        // on a merge, and would equally hide a real new gap behind an
        // unrelated function of the same name in another file.
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("baseline.json");

        let mut previous = sample_audit();
        previous.tests.untested_entrypoints = vec![untested("label", "src/a.rs")];
        std::fs::write(&path, previous.to_json().expect("json")).expect("write");

        // Same name, different file: this is a genuinely new gap.
        let mut current = sample_audit();
        current.tests.untested_entrypoints = vec![
            untested("label", "src/a.rs"),
            untested("label", "src/b.rs"),
        ];
        let reason = evaluate_gates(&current, None, false, Some(path.clone())).expect("gate");
        assert!(
            reason.is_some_and(|text| text.contains("src/b.rs")),
            "a new `label` in another file must be reported, and named by file"
        );

        // Unchanged: the same `label` in the same file is not new.
        let mut unchanged = sample_audit();
        unchanged.tests.untested_entrypoints = vec![untested("label", "src/a.rs")];
        assert!(
            evaluate_gates(&unchanged, None, false, Some(path))
                .expect("gate")
                .is_none(),
            "an unchanged gap must not be reported as new"
        );
    }

    fn untested(name: &str, file: &str) -> testing::UntestedEntrypoint {
        testing::UntestedEntrypoint {
            name: name.to_owned(),
            kind: crate::audit::EntryPointKind::Http,
            route: None,
            file: file.to_owned(),
            line_range: crate::index::LineRange { start: 1, end: 1 },
            handler_resolved: true,
        }
    }

    #[test]
    fn a_missing_baseline_is_an_error_not_a_silent_pass() {
        let audit = sample_audit();
        let result = evaluate_gates(&audit, None, false, Some(PathBuf::from("nope.json")));
        assert!(
            matches!(result, Err(CliError::Baseline { .. })),
            "a baseline that cannot be read must fail loudly"
        );
    }

    #[test]
    fn no_gates_requested_means_no_gate_trips() {
        let audit = sample_audit();
        assert!(evaluate_gates(&audit, None, false, None)
            .expect("gate")
            .is_none());
    }
}
