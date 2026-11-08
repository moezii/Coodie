//! The client-facing report.
//!
//! The terminal output is for the engineer who ran the audit. This is for the
//! person they have to convince: a founder, an acquirer, a security reviewer.
//!
//! Two rules shape it. Every number is traceable to a file and line, because an
//! audit nobody can check is worth nothing. And findings stay separated from
//! limitations — the report never implies we proved something we inferred.

use std::fmt::Write as _;

use super::{AuditReport, SupportLevel, VerdictStatus};

/// Render the audit as Markdown suitable for sending to a client.
pub fn markdown(report: &AuditReport) -> String {
    let mut out = String::new();
    let coverage = &report.coverage;
    let tests = &report.tests;
    let verdict = &report.verdict;

    let project = report
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "this repository".to_owned());

    let _ = writeln!(out, "# Software Audit — {project}\n");
    let _ = writeln!(
        out,
        "Static, evidence-based audit. Every finding below cites a file and line \
         and can be checked by hand. No code was executed.\n"
    );

    // ---- Headline -----------------------------------------------------------
    let _ = writeln!(out, "## Summary\n");
    let _ = writeln!(out, "| | |");
    let _ = writeln!(out, "|---|---|");
    let _ = writeln!(out, "| **Verdict** | {} |", verdict_sentence(verdict.status));
    let _ = writeln!(
        out,
        "| **Score** | {:.0} / 100 ({}) |",
        report.score.score, report.score.grade
    );
    let _ = writeln!(out, "| Files analysed | {} |", report.files);
    let _ = writeln!(out, "| Functions and components | {} |", report.symbols);
    let _ = writeln!(
        out,
        "| Ways into the system | {} |",
        coverage.external_entrypoint_count
    );
    let _ = writeln!(
        out,
        "| Code accounted for | {:.0}% |",
        coverage.coverage_percent
    );
    let _ = writeln!(
        out,
        "| Public surface covered by a test | {} of {} ({:.0}%) |\n",
        tests.tested_entrypoints,
        tests.external_entrypoints,
        tests.entrypoint_test_coverage * 100.0
    );

    // ---- What the software does --------------------------------------------
    let _ = writeln!(out, "## What this software exposes\n");
    let _ = writeln!(
        out,
        "These are the {} places the outside world can reach the system.\n",
        coverage.external_entrypoint_count
    );

    let mut by_kind: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for entrypoint in &report.entrypoints {
        *by_kind.entry(entrypoint.kind.label()).or_insert(0) += 1;
    }
    let _ = writeln!(out, "| Type | Count |");
    let _ = writeln!(out, "|---|---|");
    for (kind, count) in &by_kind {
        let _ = writeln!(out, "| {kind} | {count} |");
    }
    let _ = writeln!(out);

    // ---- Plain English ------------------------------------------------------
    if let Some(narrative) = &report.narrative {
        if !narrative.features.is_empty() {
            let _ = writeln!(out, "## What this software does\n");
            let _ = writeln!(
                out,
                "Described from the verified inventory above — every endpoint named here was \
                 proven to exist.\n"
            );
            for feature in &narrative.features {
                let _ = writeln!(out, "### {}\n", feature.area);
                let _ = writeln!(out, "{}\n", feature.summary);
                let _ = writeln!(
                    out,
                    "_{} entrypoint(s){}_\n",
                    feature.entrypoints.len(),
                    if feature.tested {
                        ", covered by a test"
                    } else {
                        ", **no test reaches this**"
                    }
                );
            }
        }

        if !narrative.unfulfilled_promises.is_empty() {
            let _ = writeln!(out, "## Claims the code does not appear to keep\n");
            let counts = report
                .claims
                .counts()
                .iter()
                .map(|(source, count)| format!("{count} {}", source.label()))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                out,
                "{} claim(s) were read from across the repository ({}). These are the ones with \
                 nothing in the code that appears to implement them.\n",
                report.claims.len(),
                counts
            );
            let _ = writeln!(out, "| Claim | Stated in | Why it looks unmet |");
            let _ = writeln!(out, "|---|---|---|");
            for promise in &narrative.unfulfilled_promises {
                let _ = writeln!(
                    out,
                    "| {} | `{}` | {} |",
                    promise.promise,
                    if promise.source.is_empty() {
                        "—"
                    } else {
                        &promise.source
                    },
                    promise.reason
                );
            }
            let _ = writeln!(out);
        }
    }

    // ---- Risk: untested entrypoints ----------------------------------------
    let _ = writeln!(out, "## Risk 1 — public surface with no test\n");
    if tests.untested_entrypoints.is_empty() {
        let _ = writeln!(out, "Every entrypoint is reached by at least one test.\n");
    } else {
        let _ = writeln!(
            out,
            "**{} of {} entrypoints and exported functions are not reached by any test.** If one of these \
             breaks, nothing in the test suite will notice.\n",
            tests.untested_entrypoints.len(),
            tests.external_entrypoints
        );
        let _ = writeln!(out, "| Entrypoint | Location |");
        let _ = writeln!(out, "|---|---|");
        for entrypoint in tests.untested_entrypoints.iter().take(40) {
            let _ = writeln!(
                out,
                "| `{}` | `{}:{}` |",
                entrypoint.name, entrypoint.file, entrypoint.line_range.start
            );
        }
        if tests.untested_entrypoints.len() > 40 {
            let _ = writeln!(
                out,
                "\n_and {} more._\n",
                tests.untested_entrypoints.len() - 40
            );
        }
        let _ = writeln!(out);
    }

    // ---- Risk: tests that cannot fail --------------------------------------
    let _ = writeln!(out, "## Risk 2 — tests that cannot fail\n");
    if tests.assertion_free_tests.is_empty() {
        let _ = writeln!(out, "Every test function contains at least one assertion.\n");
    } else {
        let _ = writeln!(
            out,
            "**{} test(s) call code but check no result.** These pass whether the \
             code works or not, so they report safety that does not exist.\n",
            tests.assertion_free_tests.len()
        );
        let _ = writeln!(out, "| Test | Location |");
        let _ = writeln!(out, "|---|---|");
        for test in &tests.assertion_free_tests {
            let _ = writeln!(
                out,
                "| `{}` | `{}:{}` |",
                test.name, test.file, test.line_range.start
            );
        }
        let _ = writeln!(out);
    }

    // ---- Risk: unexplained code --------------------------------------------
    let _ = writeln!(out, "## Risk 3 — code nobody can account for\n");
    if coverage.counts.dead == 0 {
        let _ = writeln!(
            out,
            "No unreferenced code was found. Every function is either reachable, \
             a test, configuration, or generated.\n"
        );
    } else {
        let _ = writeln!(
            out,
            "**{} function(s) are unreachable and unreferenced.** Nothing calls them \
             and no entrypoint reaches them. Each is either dead code to remove, or \
             live code wired up in a way that is not visible in the source.\n",
            coverage.counts.dead
        );
        let dead: Vec<_> = coverage
            .unaccounted
            .iter()
            .filter(|finding| finding.classification == super::Classification::Dead)
            .collect();
        let unused_types = dead.iter().filter(|finding| finding.is_type).count();
        if unused_types > 0 {
            let _ = writeln!(
                out,
                "{unused_types} of these are **types** — models, interfaces, or classes that are \
                 declared and never used anywhere.\n"
            );
        }
        let _ = writeln!(out, "| What | Name | Location |");
        let _ = writeln!(out, "|---|---|---|");
        for finding in dead.iter().take(30) {
            let _ = writeln!(
                out,
                "| {} | `{}` | `{}:{}` |",
                if finding.is_type { "type" } else { "function" },
                finding.name,
                finding.file,
                finding.line_range.start
            );
        }
        let _ = writeln!(out);
    }

    // ---- Honest limits ------------------------------------------------------
    let _ = writeln!(out, "## Not proven either way\n");
    if verdict.limitations.is_empty() {
        let _ = writeln!(out, "None. The whole codebase resolved.\n");
    } else {
        let _ = writeln!(
            out,
            "Code that reads as unreachable is not always unused — it may be reached through \
             an interface, a framework callback, or dependency injection, none of which have a \
             visible caller. Rather than report these as defects, they are listed here as \
             unproven:\n"
        );
        for limitation in &verdict.limitations {
            let _ = writeln!(out, "- {limitation}");
        }
        let mentioned: Vec<_> = coverage
            .unaccounted
            .iter()
            .filter(|finding| finding.reachability == super::Reachability::MentionedNotTraced)
            .collect();
        if !mentioned.is_empty() {
            let _ = writeln!(
                out,
                "
**Mentioned but not traced.** Nothing the engine follows reaches these,                  but each name is written somewhere else, so they are not called dead.                  Check the mention: if it is a use, the code is live; if not, it is dead.
"
            );
            let _ = writeln!(out, "| Name | Defined at | First mention |");
            let _ = writeln!(out, "|---|---|---|");
            for finding in mentioned.iter().take(30) {
                let _ = writeln!(
                    out,
                    "| `{}` | `{}:{}` | {} |",
                    finding.name,
                    finding.file,
                    finding.line_range.start,
                    finding
                        .mentioned_at
                        .as_deref()
                        .map(|site| format!("`{site}`"))
                        .unwrap_or_else(|| "(name has no searchable word)".to_owned())
                );
            }
            if mentioned.len() > 30 {
                let _ = writeln!(out, "
…and {} more in the JSON output.", mentioned.len() - 30);
            }
        }
        let _ = writeln!(
            out,
            "\nConfidence in this analysis: **{:.0}%**. Raising it requires runtime \
             evidence — running the test suite with coverage instrumentation.\n",
            verdict.confidence * 100.0
        );
    }

    // ---- Coverage of languages ---------------------------------------------
    let _ = writeln!(out, "## Analysis coverage by language\n");
    let _ = writeln!(out, "| Language | Files | Functions | Depth |");
    let _ = writeln!(out, "|---|---|---|---|");
    for (language, support) in &report.languages {
        if support.files == 0 {
            continue;
        }
        let depth = match support.support {
            SupportLevel::Entrypoints => "entrypoints + calls",
            SupportLevel::Baseline => "symbols only",
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            language, support.files, support.symbols, depth
        );
    }
    let _ = writeln!(out);
    if !report.vendored.is_empty() {
        let _ = writeln!(
            out,
            "Vendored third-party code, excluded from this audit: {}.
",
            report
                .vendored
                .iter()
                .map(|dir| format!("`{dir}/`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // ---- Recommendations ----------------------------------------------------
    let _ = writeln!(out, "## Recommended actions\n");
    let mut step = 1;
    if !tests.assertion_free_tests.is_empty() {
        let _ = writeln!(
            out,
            "{step}. **Fix the tests that cannot fail.** Add assertions so they check \
             the result they compute. Highest priority — these actively mislead."
        );
        step += 1;
    }
    if !tests.untested_entrypoints.is_empty() {
        let _ = writeln!(
            out,
            "{step}. **Cover the entrypoints.** Start with the ones handling money, \
             authentication, or personal data. One test per entrypoint that asserts \
             the response is enough to catch a total break."
        );
        step += 1;
    }
    if coverage.counts.dead > 0 {
        let _ = writeln!(
            out,
            "{step}. **Resolve the unreferenced functions.** Delete what is dead; \
             document how the rest is reached."
        );
        step += 1;
    }
    let _ = writeln!(
        out,
        "{step}. **Re-run this audit on every change** to see whether these numbers \
         are moving in the right direction."
    );

    out
}

fn verdict_sentence(status: VerdictStatus) -> &'static str {
    match status {
        VerdictStatus::Clean => "**CLEAN** — analysis was confident and found no issues",
        VerdictStatus::FindingsFound => "**FINDINGS** — issues found, listed below",
        VerdictStatus::Inconclusive => {
            "**INCONCLUSIVE** — too little could be resolved to judge this codebase"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::audit_repository;
    use std::fs;
    use tempfile::tempdir;

    /// A small service with one covered route, one uncovered route, and an
    /// orphan — enough for every section of the report to have something real.
    fn sample_repository() -> tempfile::TempDir {
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        fs::create_dir_all(root.join("app")).expect("dirs");
        fs::create_dir_all(root.join("tests")).expect("dirs");
        fs::write(
            root.join("app/main.py"),
            "from fastapi import FastAPI\n\
             app = FastAPI()\n\n\
             @app.get(\"/users\")\n\
             def list_users():\n    return fetch_users()\n\n\
             @app.post(\"/pay\")\n\
             def take_payment():\n    return charge()\n\n\
             def fetch_users():\n    return []\n\n\
             def charge():\n    return True\n\n\
             def orphaned_helper():\n    return 'nothing calls me'\n",
        )
        .expect("write");
        fs::write(
            root.join("tests/test_users.py"),
            "from app.main import fetch_users\n\n\
             def test_users():\n    assert fetch_users() == []\n",
        )
        .expect("write");
        directory
    }

    #[test]
    fn the_report_names_untested_entrypoints_rather_than_a_percentage() {
        let directory = sample_repository();
        let audit = audit_repository(directory.path()).expect("audit");
        let markdown = markdown(&audit);

        assert!(markdown.contains("Risk 1 — public surface with no test"));
        // A named endpoint is actionable in a way that "50% covered" is not.
        assert!(
            markdown.contains("POST /pay") || markdown.contains("GET /users"),
            "an untested endpoint must be named:\n{markdown}"
        );
    }

    #[test]
    fn the_report_lists_unreferenced_code_with_a_location() {
        let directory = sample_repository();
        let audit = audit_repository(directory.path()).expect("audit");
        let markdown = markdown(&audit);

        assert!(markdown.contains("Risk 3 — code nobody can account for"));
        assert!(
            markdown.contains("orphaned_helper"),
            "unreferenced code must be named:\n{markdown}"
        );
        // Every finding cites a file so a reader can check it by hand.
        assert!(markdown.contains("app/main.py"), "{markdown}");
    }

    #[test]
    fn a_report_without_descriptions_omits_the_english_sections() {
        // The model is optional. Without it the report is still complete, just
        // without prose — it must not render an empty heading.
        let directory = sample_repository();
        let audit = audit_repository(directory.path()).expect("audit");
        assert!(audit.narrative.is_none());

        let markdown = markdown(&audit);
        assert!(!markdown.contains("## What this software does"));
        assert!(!markdown.contains("## Claims the code does not appear to keep"));
        // The deterministic sections are all still there.
        assert!(markdown.contains("## Summary"));
        assert!(markdown.contains("## Recommended actions"));
    }

    #[test]
    fn every_headline_number_appears_in_the_summary() {
        let directory = sample_repository();
        let audit = audit_repository(directory.path()).expect("audit");
        let markdown = markdown(&audit);

        assert!(markdown.contains("Files analysed"));
        assert!(markdown.contains("Ways into the system"));
        assert!(markdown.contains("Code accounted for"));
        assert!(markdown.contains("Public surface covered by a test"));
        assert!(markdown.contains("Verdict"));
    }

    #[test]
    fn report_states_findings_and_limits_separately() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        fs::create_dir_all(root.join("app")).expect("dirs");
        fs::write(
            root.join("app/main.py"),
            "from fastapi import FastAPI\napp = FastAPI()\n\n@app.get(\"/users\")\ndef list_users():\n    return []\n",
        )
        .expect("write");

        let audit = audit_repository(root).expect("audit");
        let markdown = markdown(&audit);

        assert!(markdown.contains("# Software Audit"));
        assert!(markdown.contains("Risk 1 — public surface with no test"));
        // What could not be resolved is reported apart from what was found, and
        // never as a defect in the audited code.
        assert!(markdown.contains("Not proven either way"));
        assert!(
            markdown.contains("listed here as unproven")
                || markdown.contains("None. The whole codebase resolved."),
            "unresolved code must not read as a finding against the repository"
        );
        assert!(markdown.contains("Recommended actions"));
    }
}
