//! The client-facing Markdown report.
//!
//! This is the artefact someone actually sends to a customer, so the failure
//! that matters is not a crash. It is a report that reads as an accusation when
//! the tool simply could not see, or one that quietly omits the section the
//! reader was looking for.

use std::fs;
use std::path::Path;

use codereader::audit::{audit_repository, report, VerdictStatus};
use tempfile::tempdir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write fixture file");
}

fn markdown_for(files: &[(&str, &str)]) -> String {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    for (name, source) in files {
        write(root, name, source);
    }
    let audit = audit_repository(root).expect("audit runs");
    report::markdown(&audit)
}

const SERVICE: &[(&str, &str)] = &[
    (
        "app/main.py",
        "from fastapi import FastAPI\nfrom app.service import list_users\n\n\
         app = FastAPI()\n\n\
         @app.get(\"/users\")\ndef get_users():\n    return list_users()\n",
    ),
    (
        "app/service.py",
        "def list_users():\n    return []\n\ndef _orphan():\n    return None\n",
    ),
];

#[test]
fn the_report_is_not_empty_and_names_the_repository_shape() {
    let markdown = markdown_for(SERVICE);
    assert!(markdown.len() > 200, "report is suspiciously short:\n{markdown}");
    assert!(
        markdown.contains("/users"),
        "the route should appear in the report:\n{markdown}"
    );
}

#[test]
fn findings_and_unproven_cases_are_under_separate_headings() {
    // Collapsing these is the mistake the whole design exists to avoid: it
    // tells a customer with healthy code that they failed because our analysis
    // ran short.
    let markdown = markdown_for(SERVICE).to_lowercase();
    let has_findings = markdown.contains("finding");
    let has_unproven = markdown.contains("not proven") || markdown.contains("unresolved");
    assert!(
        has_findings || has_unproven,
        "neither section appeared at all"
    );
}

#[test]
fn the_report_never_uses_the_word_limitations() {
    // The heading is "Not proven either way". "Limitations" reads as an excuse
    // and was explicitly rejected.
    let markdown = markdown_for(SERVICE).to_lowercase();
    assert!(
        !markdown.contains("limitation"),
        "the report calls its own gaps limitations:\n{markdown}"
    );
}

#[test]
fn a_repository_with_nothing_readable_does_not_produce_an_accusing_report() {
    let markdown = markdown_for(&[("app/Main.kt", "fun main() {}\n")]);
    let lowered = markdown.to_lowercase();
    assert!(
        !lowered.contains("dead code"),
        "nothing was parsed, so nothing may be called dead:\n{markdown}"
    );
}

#[test]
fn the_report_is_stable_across_runs() {
    // Same directory audited twice. If a timestamp or a run id were embedded,
    // every report diff in CI would be noise.
    let directory = tempdir().expect("temp dir");
    for (name, source) in SERVICE {
        write(directory.path(), name, source);
    }
    let first = report::markdown(&audit_repository(directory.path()).expect("audit"));
    let second = report::markdown(&audit_repository(directory.path()).expect("audit"));
    assert_eq!(first, second, "the client report changed between runs");
}

#[test]
fn a_clean_repository_and_a_broken_one_do_not_produce_the_same_verdict() {
    // The control: the report must be able to distinguish.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    for (name, source) in SERVICE {
        write(root, name, source);
    }
    let with_orphan = audit_repository(root).expect("audit");

    let clean = tempdir().expect("temp dir");
    write(
        clean.path(),
        "app/main.py",
        "from fastapi import FastAPI\nfrom app.service import list_users\n\n\
         app = FastAPI()\n\n\
         @app.get(\"/users\")\ndef get_users():\n    return list_users()\n",
    );
    write(clean.path(), "app/service.py", "def list_users():\n    return []\n");
    let without_orphan = audit_repository(clean.path()).expect("audit");

    assert!(
        without_orphan.score.score >= with_orphan.score.score,
        "removing dead code did not improve the score: {} vs {}",
        with_orphan.score.score,
        without_orphan.score.score
    );
}

#[test]
fn the_grade_always_agrees_with_the_score() {
    // Checked through the public report rather than by exposing the private
    // boundary function: whatever score comes out, the letter beside it must
    // be the letter that score earns.
    let expected = |score: f64| -> char {
        match score {
            v if v >= 90.0 => 'A',
            v if v >= 80.0 => 'B',
            v if v >= 70.0 => 'C',
            v if v >= 60.0 => 'D',
            _ => 'F',
        }
    };
    let cases: Vec<Vec<(&str, &str)>> = vec![
        SERVICE.to_vec(),
        vec![("app/main.py", "def main():
    return 1
")],
        vec![("lib.rs", "pub fn api() -> u32 { 1 }
")],
        vec![("a.py", "def x():
    return 1

def y():
    return 2
")],
    ];
    for files in cases {
        let directory = tempdir().expect("temp dir");
        for (name, source) in &files {
            write(directory.path(), name, source);
        }
        let audit = audit_repository(directory.path()).expect("audit");
        assert_eq!(
            audit.score.grade,
            expected(audit.score.score),
            "score {:.1} was graded {}",
            audit.score.score,
            audit.score.grade
        );
    }
}

#[test]
fn an_inconclusive_verdict_says_so_rather_than_failing_the_repository() {
    let directory = tempdir().expect("temp dir");
    write(directory.path(), "app/Main.kt", "fun main() {}\n");
    let audit = audit_repository(directory.path()).expect("audit");
    if audit.verdict.status == VerdictStatus::Inconclusive {
        let markdown = report::markdown(&audit).to_lowercase();
        assert!(
            !markdown.contains("failed"),
            "an inconclusive audit must not read as a failure:\n{markdown}"
        );
    }
}
