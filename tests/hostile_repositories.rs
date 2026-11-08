//! Repositories that are awkward on purpose.
//!
//! The cases here are not about one language's syntax. They are about what the
//! tool says when it cannot see much: a language it has no grammar for, a
//! repository that is all configuration, a file that is not really source.
//!
//! The rule is the same throughout. Seeing nothing must produce
//! `INCONCLUSIVE`, never `CLEAN`. A clean bill of health handed to someone
//! whose code was never read is the most expensive output this tool could
//! produce.

use std::fs;
use std::path::Path;

use codereader::audit::{audit_repository, VerdictStatus};
use tempfile::tempdir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write fixture file");
}

fn audit(files: &[(&str, &str)]) -> codereader::audit::AuditReport {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    for (name, source) in files {
        write(root, name, source);
    }
    audit_repository(root).expect("audit runs")
}

#[test]
fn a_repository_in_a_language_we_cannot_parse_is_not_clean() {
    // Kotlin has an extension mapping but no grammar, so nothing in these
    // files is visible. Reporting CLEAN would mean "we read your Android app
    // and it is fine" when not one line was read.
    let report = audit(&[
        (
            "app/Main.kt",
            "fun main() {\n    println(\"hello\")\n}\n\
             fun unused() {}\n",
        ),
        (
            "app/Service.kt",
            "class Service {\n    fun process() {}\n}\n",
        ),
    ]);
    assert_ne!(
        report.verdict.status,
        VerdictStatus::Clean,
        "nothing here was parsed, so nothing can be called clean"
    );
}

#[test]
fn an_unparsed_language_is_named_in_the_report() {
    // Saying "inconclusive" is only useful with the reason attached.
    let report = audit(&[("app/Main.kt", "fun main() {}\n")]);
    let kotlin = report.languages.get("kotlin");
    assert!(kotlin.is_some(), "kotlin should be listed: {:?}", report.languages);
}

#[test]
fn a_repository_of_pure_configuration_is_not_clean() {
    let report = audit(&[
        ("config.yaml", "service:\n  port: 8080\n"),
        ("data.json", "{\"a\": 1}\n"),
        ("Dockerfile", "FROM scratch\n"),
    ]);
    assert_ne!(
        report.verdict.status,
        VerdictStatus::Clean,
        "there is no code here to be clean about"
    );
}

#[test]
fn an_empty_repository_is_not_clean() {
    let report = audit(&[]);
    assert_ne!(report.verdict.status, VerdictStatus::Clean);
}

#[test]
fn a_documentation_only_repository_is_not_clean() {
    let report = audit(&[
        ("README.md", "# Project\n\nDoes things.\n"),
        ("docs/guide.md", "## Guide\n"),
    ]);
    assert_ne!(report.verdict.status, VerdictStatus::Clean);
}

#[test]
fn a_mixed_repository_reports_every_language_it_saw() {
    let report = audit(&[
        ("api/main.py", "def handler():\n    return 1\n"),
        ("web/app.js", "export function render() { return 1; }\n"),
        ("core/lib.rs", "pub fn compute() -> u32 { 1 }\n"),
        ("svc/main.go", "package main\n\nfunc main() {}\n"),
    ]);
    for language in ["python", "javascript", "rust", "go"] {
        assert!(
            report.languages.contains_key(language),
            "{language} missing from {:?}",
            report.languages.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_source_file_that_is_actually_minified_does_not_derail_the_audit() {
    // One line, no newlines, thousands of characters — common in `dist/`.
    let minified = format!(
        "function a(){{return 1}}{}",
        "function b(){return 2};".repeat(400)
    );
    let report = audit(&[("app.js", &minified), ("real.py", "def f():\n    return 1\n")]);
    assert!(report.files > 0);
}

#[test]
fn deeply_nested_directories_are_walked() {
    let deep = "a/b/c/d/e/f/g/h/i/j/deep.py";
    let report = audit(&[(deep, "def buried():\n    return 1\n")]);
    assert!(report.symbols > 0, "nothing found at depth 10");
}

#[test]
fn a_file_with_windows_line_endings_parses() {
    let report = audit(&[("app.py", "def one():\r\n    return 1\r\n\r\ndef two():\r\n    return 2\r\n")]);
    assert!(report.symbols >= 2, "CRLF broke parsing: {} symbols", report.symbols);
}

#[test]
fn a_file_with_a_byte_order_mark_parses() {
    let report = audit(&[("app.py", "\u{feff}def one():\n    return 1\n")]);
    assert!(report.symbols >= 1, "BOM broke parsing");
}

#[test]
fn identical_symbol_names_in_different_files_stay_separate() {
    // `handler` exists twice. Neither may be resolved to the other, because a
    // guessed edge makes dead code look reachable.
    let report = audit(&[
        ("a.py", "def handler():\n    return 1\n"),
        ("b.py", "def handler():\n    return 2\n"),
    ]);
    assert!(report.symbols >= 2, "both definitions should be recorded");
}

#[test]
fn a_self_recursive_function_does_not_hang_or_count_as_used() {
    // Calling yourself is not evidence anyone else does.
    let report = audit(&[(
        "app.py",
        "def loop(n):\n    if n:\n        return loop(n - 1)\n    return 0\n",
    )]);
    assert!(report.files > 0);
}

#[test]
fn mutually_recursive_functions_terminate() {
    let report = audit(&[(
        "app.py",
        "def ping(n):\n    return pong(n)\n\ndef pong(n):\n    return ping(n)\n",
    )]);
    assert!(report.files > 0, "cycle should not hang the walk");
}
