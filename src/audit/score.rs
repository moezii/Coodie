//! Scoring and the verdict.
//!
//! Two separate questions, deliberately never merged:
//!
//! 1. **What did we find wrong with your code?** — findings.
//! 2. **How much of your code could we actually check?** — completeness.
//!
//! Collapsing them produces the worst possible outcome: a repository scoring 98
//! out of 100 and still stamped a failure, because a handful of symbols the
//! analysis could not trace were counted as defects in the customer's code.
//! They are not defects. They are the limit of what static analysis can see.
//!
//! So: `Inconclusive` is a statement about *us*. `FindingsFound` is a statement
//! about *them*. Never let one masquerade as the other.

use serde::{Deserialize, Serialize};

use super::coverage::CoverageReport;

/// Coverage leads: unexplained code is the trap this engine exists to catch.
const WEIGHT_COVERAGE: f64 = 0.40;
/// How much reachability we could actually resolve.
const WEIGHT_CLARITY: f64 = 0.30;
/// Whether the way in is discoverable at all.
const WEIGHT_STRUCTURE: f64 = 0.15;
/// Whether anything is tested.
const WEIGHT_TESTS: f64 = 0.15;

/// Below this share of resolvable nodes we do not trust our own analysis.
const MIN_CONFIDENCE: f64 = 0.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deduction {
    pub code: String,
    pub detail: String,
    pub points: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Score {
    pub score: f64,
    pub grade: char,
    pub coverage: f64,
    pub clarity: f64,
    pub structure: f64,
    pub tests: f64,
    pub deductions: Vec<Deduction>,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerdictStatus {
    /// Analysis was confident and found nothing unaccounted for.
    Clean,
    /// Analysis was confident and found real problems in the code.
    FindingsFound,
    /// Analysis could not see enough to judge. This is about our limits, not
    /// their code, and must never be reported as a failure of the repository.
    Inconclusive,
}

impl VerdictStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "CLEAN",
            Self::FindingsFound => "FINDINGS",
            Self::Inconclusive => "INCONCLUSIVE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub status: VerdictStatus,
    /// Problems in the audited code. Actionable by the customer.
    pub findings: Vec<String>,
    /// Things this engine could not determine. Actionable by us.
    pub limitations: Vec<String>,
    /// How much of the repository the analysis could actually resolve, 0-1.
    pub confidence: f64,
}

pub fn score(coverage: &CoverageReport) -> Score {
    let meaningful = coverage.meaningful_nodes.max(1) as f64;

    let coverage_ratio = coverage.accounted_nodes as f64 / meaningful;
    let unresolved = coverage.counts.unknown as f64;
    let clarity_ratio = 1.0 - (unresolved / meaningful);

    // Structure: can we see how the outside world gets in at all? A library has
    // no routes; its public API is the way in. Scoring on routes alone gave
    // every package zero for being a package.
    let structure_ratio = if coverage.external_entrypoint_count > 0
        || coverage.public_api_nodes > 0
    {
        1.0
    } else if coverage.entrypoint_count > 0 {
        0.5
    } else {
        0.0
    };

    // Tests: is any of this exercised?
    let tests_ratio = if coverage.counts.test == 0 {
        0.0
    } else {
        let feature_like = (coverage.counts.feature + coverage.counts.utility).max(1) as f64;
        (coverage.counts.test as f64 / feature_like).min(1.0)
    };

    let raw = WEIGHT_COVERAGE * coverage_ratio
        + WEIGHT_CLARITY * clarity_ratio.max(0.0)
        + WEIGHT_STRUCTURE * structure_ratio
        + WEIGHT_TESTS * tests_ratio;
    let total = round2((raw * 100.0).clamp(0.0, 100.0));

    let mut deductions = Vec::new();
    let mut suggestions = Vec::new();

    if coverage.counts.dead > 0 {
        deductions.push(Deduction {
            code: "dead_code".to_owned(),
            detail: format!(
                "{} symbol(s) are not referenced by anything and not reached by any entrypoint.",
                coverage.counts.dead
            ),
            points: round2(WEIGHT_COVERAGE * 100.0 * (coverage.counts.dead as f64 / meaningful)),
        });
        suggestions.push(
            "Delete the unreferenced symbols, or connect them to an entrypoint if they are meant to be live."
                .to_owned(),
        );
    }

    if coverage.counts.unknown > 0 {
        deductions.push(Deduction {
            code: "unresolved_reachability".to_owned(),
            detail: format!(
                "{} symbol(s) are referenced but cannot be traced back to any entrypoint, startup path, or test.",
                coverage.counts.unknown
            ),
            points: round2(WEIGHT_CLARITY * 100.0 * (unresolved / meaningful)),
        });
        suggestions.push(
            "Trace these by hand — they are reached through dynamic dispatch, DI, or a framework this engine does not model yet."
                .to_owned(),
        );
    }

    if coverage.external_entrypoint_count == 0 && coverage.public_api_nodes == 0 {
        deductions.push(Deduction {
            code: "no_external_entrypoints".to_owned(),
            detail:
                "No externally reachable entrypoints were detected, so nothing can be proven reachable."
                    .to_owned(),
            points: round2(WEIGHT_STRUCTURE * 100.0),
        });
        suggestions.push(
            "If this is a library, that is expected. If it is a service, its framework may not be supported yet."
                .to_owned(),
        );
    }

    if coverage.counts.test == 0 {
        deductions.push(Deduction {
            code: "no_tests".to_owned(),
            detail: "No test code was found.".to_owned(),
            points: round2(WEIGHT_TESTS * 100.0),
        });
        suggestions.push("Add tests that exercise the externally reachable entrypoints.".to_owned());
    }

    Score {
        score: total,
        grade: grade_for(total),
        coverage: round2(coverage_ratio),
        clarity: round2(clarity_ratio.max(0.0)),
        structure: round2(structure_ratio),
        tests: round2(tests_ratio),
        deductions,
        suggestions,
    }
}

pub fn verdict(coverage: &CoverageReport, score: &Score) -> Verdict {
    let meaningful = coverage.meaningful_nodes;
    let confidence = analysis_confidence(coverage);

    let mut findings = Vec::new();
    let mut limitations = Vec::new();

    if coverage.counts.dead > 0 {
        findings.push(format!(
            "{} symbol(s) are unreachable and unreferenced — likely dead code.",
            coverage.counts.dead
        ));
    }
    if coverage.counts.unknown > 0 {
        limitations.push(format!(
            "{} symbol(s) are referenced but could not be traced to any root. Static analysis cannot prove these either way.",
            coverage.counts.unknown
        ));
    }
    if coverage.mentioned_not_traced > 0 {
        limitations.push(format!(
            "{} of those symbol(s) have no traced caller at all, but their name appears elsewhere in the repository (a macro, build file, config, script, string or another language). They are not reported as dead.",
            coverage.mentioned_not_traced
        ));
    }
    if coverage.dynamic_only_nodes > 0 {
        limitations.push(format!(
            "{} symbol(s) are reachable only through polymorphic dispatch. They are counted as covered, but which implementation runs was inferred, not proven.",
            coverage.dynamic_only_nodes
        ));
    }
    // No routes and no public API means nothing can be reached, which is a
    // limit of the analysis. No routes but a public API is just a library, and
    // is judged normally.
    if coverage.external_entrypoint_count == 0 && coverage.public_api_nodes == 0 && meaningful > 0 {
        limitations.push(
            "No entrypoints and no public API were detected, so nothing could be traced. The framework in use may not be modelled yet."
                .to_owned(),
        );
    }
    if coverage.counts.test == 0 && meaningful > 0 {
        findings.push("No tests were found anywhere in the repository.".to_owned());
    }

    // The decisive rule. If we could not see enough, we say so about ourselves
    // instead of reporting the repository as failed.
    let status = if meaningful == 0
        || confidence < MIN_CONFIDENCE
        || (coverage.external_entrypoint_count == 0 && coverage.public_api_nodes == 0)
    {
        VerdictStatus::Inconclusive
    } else if findings.is_empty() {
        VerdictStatus::Clean
    } else {
        VerdictStatus::FindingsFound
    };

    // Only say the confidence was too low when it actually was. Printing
    // "confidence is 100%, below the 50% needed" made the report contradict
    // itself on every library.
    if status == VerdictStatus::Inconclusive && confidence < MIN_CONFIDENCE {
        limitations.push(format!(
            "Only {:.0}% of this codebase could be resolved, below the {:.0}% needed to judge it.",
            confidence * 100.0,
            MIN_CONFIDENCE * 100.0
        ));
    }

    let _ = score;

    Verdict {
        status,
        findings,
        limitations,
        confidence: round2(confidence),
    }
}

/// How much of the repository the analysis could actually resolve.
///
/// Nodes we classified confidently count toward confidence; `Unknown` counts
/// against it. Dead code does *not* count against confidence — "this is
/// unreferenced" is a conclusion, not a failure to reach one.
fn analysis_confidence(coverage: &CoverageReport) -> f64 {
    if coverage.meaningful_nodes == 0 {
        return 0.0;
    }
    let resolved = coverage.meaningful_nodes.saturating_sub(coverage.counts.unknown);
    round2(resolved as f64 / coverage.meaningful_nodes as f64)
}

fn grade_for(score: f64) -> char {
    match score {
        value if value >= 90.0 => 'A',
        value if value >= 80.0 => 'B',
        value if value >= 70.0 => 'C',
        value if value >= 60.0 => 'D',
        _ => 'F',
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::coverage::ClassificationCounts;

    fn report(
        meaningful: usize,
        accounted: usize,
        counts: ClassificationCounts,
        external_entrypoints: usize,
    ) -> CoverageReport {
        CoverageReport {
            meaningful_nodes: meaningful,
            accounted_nodes: accounted,
            unaccounted_nodes: meaningful - accounted,
            coverage_percent: if meaningful == 0 {
                100.0
            } else {
                (accounted as f64 / meaningful as f64) * 100.0
            },
            entrypoint_count: external_entrypoints,
            external_entrypoint_count: external_entrypoints,
            counts,
            dynamic_only_nodes: 0,
            public_api_nodes: 0,
            public_unused: Vec::new(),
            mentioned_not_traced: 0,
            unaccounted: Vec::new(),
        }
    }

    #[test]
    fn a_healthy_repository_is_clean_not_failed() {
        let counts = ClassificationCounts {
            feature: 8,
            test: 2,
            ..Default::default()
        };
        let coverage = report(10, 10, counts, 3);
        let score = score(&coverage);
        let verdict = verdict(&coverage, &score);

        assert_eq!(verdict.status, VerdictStatus::Clean);
        assert!(verdict.findings.is_empty());
    }

    #[test]
    fn dead_code_is_a_finding_about_their_code() {
        let counts = ClassificationCounts {
            feature: 5,
            test: 2,
            dead: 3,
            ..Default::default()
        };
        let coverage = report(10, 7, counts, 2);
        let score = score(&coverage);
        let verdict = verdict(&coverage, &score);

        assert_eq!(verdict.status, VerdictStatus::FindingsFound);
        assert!(verdict.findings.iter().any(|f| f.contains("dead code")));
    }

    #[test]
    fn unresolvable_code_is_our_limitation_not_their_failure() {
        let counts = ClassificationCounts {
            feature: 2,
            test: 1,
            unknown: 8,
            ..Default::default()
        };
        let coverage = report(11, 3, counts, 1);
        let score = score(&coverage);
        let verdict = verdict(&coverage, &score);

        // We could only resolve 20% of the graph. That is a statement about the
        // engine, so the verdict must be inconclusive rather than a failure.
        assert_eq!(verdict.status, VerdictStatus::Inconclusive);
        assert!(verdict.findings.is_empty());
        assert!(!verdict.limitations.is_empty());
    }

    #[test]
    fn no_entrypoints_means_inconclusive_never_zero_coverage_failure() {
        let counts = ClassificationCounts {
            dead: 20,
            ..Default::default()
        };
        let coverage = report(20, 0, counts, 0);
        let score = score(&coverage);
        let verdict = verdict(&coverage, &score);

        // Without entrypoints everything *looks* dead. Blaming the repository
        assert_eq!(verdict.status, VerdictStatus::Inconclusive);
        assert!(verdict
            .limitations
            .iter()
            .any(|limitation| limitation.contains("No entrypoints and no public API")));
    }

    #[test]
    fn score_rewards_coverage_most() {
        let strong = report(
            10,
            10,
            ClassificationCounts {
                feature: 5,
                test: 5,
                ..Default::default()
            },
            2,
        );
        let weak = report(
            10,
            3,
            ClassificationCounts {
                feature: 2,
                test: 1,
                dead: 7,
                ..Default::default()
            },
            2,
        );

        assert!(score(&strong).score > score(&weak).score);
        assert_eq!(score(&strong).grade, 'A');
    }

    #[test]
    fn empty_repository_is_inconclusive_not_perfect() {
        let coverage = report(0, 0, ClassificationCounts::default(), 0);
        let score = score(&coverage);
        assert_eq!(verdict(&coverage, &score).status, VerdictStatus::Inconclusive);
    }
}
