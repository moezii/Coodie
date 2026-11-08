//! The same repository must produce the same answer every time.
//!
//! An audit that drifts between runs cannot be used in CI, and a score that
//! moves on its own destroys trust faster than a wrong score does. Parallel
//! walking, hash-map iteration order and the content cache are all places where
//! run-to-run variation could leak in.

use std::fs;
use std::path::Path;

use codereader::audit::audit_repository;
use tempfile::tempdir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write fixture file");
}

/// A repository big enough that ordering effects have room to show up.
fn build_fixture(root: &Path) {
    write(
        root,
        "app/main.py",
        "from app.service import list_users, create_user\nfrom fastapi import FastAPI\n\n\
         app = FastAPI()\n\n\
         @app.get(\"/users\")\ndef get_users():\n    return list_users()\n\n\
         @app.post(\"/users\")\ndef post_users():\n    return create_user()\n",
    );
    write(
        root,
        "app/service.py",
        "from app.store import fetch_all, insert\n\n\
         def list_users():\n    return fetch_all()\n\n\
         def create_user():\n    return insert()\n\n\
         def _orphan():\n    return None\n",
    );
    write(
        root,
        "app/store.py",
        "def fetch_all():\n    return []\n\ndef insert():\n    return 1\n",
    );
    for index in 0..12 {
        write(
            root,
            &format!("app/mod{index}.py"),
            &format!("def helper_{index}():\n    return {index}\n"),
        );
    }
    write(
        root,
        "web/app.js",
        "export function render() { return format(1); }\n\
         function format(x) { return String(x); }\n",
    );
    write(
        root,
        "core/lib.rs",
        "pub fn compute() -> u32 { helper() }\nfn helper() -> u32 { 1 }\n",
    );
}

#[test]
fn ten_runs_over_the_same_repository_agree_exactly() {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    build_fixture(root);

    let first = audit_repository(root).expect("audit runs");
    let baseline = (
        first.files,
        first.symbols,
        first.entrypoints.len(),
        first.coverage.meaningful_nodes,
        first.coverage.unaccounted_nodes,
        format!("{:.6}", first.coverage.coverage_percent),
        format!("{:.6}", first.score.score),
        first.verdict.status,
    );

    for run in 2..=10 {
        let report = audit_repository(root).expect("audit runs");
        let actual = (
            report.files,
            report.symbols,
            report.entrypoints.len(),
            report.coverage.meaningful_nodes,
            report.coverage.unaccounted_nodes,
            format!("{:.6}", report.coverage.coverage_percent),
            format!("{:.6}", report.score.score),
            report.verdict.status,
        );
        assert_eq!(actual, baseline, "run {run} disagreed with run 1");
    }
}

#[test]
fn the_findings_list_is_in_a_stable_order() {
    // Two runs must list the same problems in the same order, or every CI diff
    // is noise.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    build_fixture(root);

    let names = |report: &codereader::audit::AuditReport| -> Vec<String> {
        report
            .coverage
            .unaccounted
            .iter()
            .map(|finding| format!("{}:{}", finding.file, finding.name))
            .collect()
    };

    let first = audit_repository(root).expect("audit");
    let second = audit_repository(root).expect("audit");
    assert_eq!(names(&first), names(&second), "finding order drifted");
}

#[test]
fn the_json_output_is_byte_identical_across_runs() {
    // This is what a CI job diffs.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    build_fixture(root);

    let first = serde_json::to_string(&audit_repository(root).expect("audit")).expect("json");
    let second = serde_json::to_string(&audit_repository(root).expect("audit")).expect("json");
    assert_eq!(first.len(), second.len(), "serialised length drifted");
    assert!(first == second, "serialised audit differed between runs");
}

#[test]
fn a_repository_reached_by_a_different_path_spelling_audits_the_same() {
    // `.` and the absolute path are the same repository.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    build_fixture(root);

    let direct = audit_repository(root).expect("audit");
    let nested = audit_repository(root.join("app").join("..")).expect("audit");
    assert_eq!(direct.symbols, nested.symbols, "path spelling changed the result");
}

#[test]
fn changing_one_file_changes_only_what_depends_on_it() {
    // The cache must not serve a stale answer after an edit.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    build_fixture(root);

    let before = audit_repository(root).expect("audit");
    write(
        root,
        "app/store.py",
        "def fetch_all():\n    return []\n\ndef insert():\n    return 1\n\
         \ndef newly_added():\n    return 2\n",
    );
    let after = audit_repository(root).expect("audit");
    assert!(
        after.symbols > before.symbols,
        "an added function was not picked up: {} then {}",
        before.symbols,
        after.symbols
    );
}

// ---------------------------------------------------------------------------
// The parse cache must be invisible
// ---------------------------------------------------------------------------
//
// Parsing is roughly 90% of a release-build audit, so its result is cached
// against each file's content hash. A cache that changes any answer is worse
// than no cache: it makes the tool fast and wrong, and the wrongness only
// appears on the second run, which is the run nobody re-checks.

#[test]
fn a_cached_audit_is_identical_to_a_fresh_one() {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    build_fixture(root);

    // First run populates the cache; second run reads it.
    let fresh = serde_json::to_string(&audit_repository(root).expect("audit")).expect("json");
    let cached = serde_json::to_string(&audit_repository(root).expect("audit")).expect("json");
    assert_eq!(fresh, cached, "the cache changed the answer");

    // And a run with the cache thrown away must agree with both.
    fs::remove_dir_all(root.join(".codereader")).ok();
    let uncached = serde_json::to_string(&audit_repository(root).expect("audit")).expect("json");
    assert_eq!(
        fresh, uncached,
        "discarding the cache changed the answer, so one of the two is wrong"
    );
}

#[test]
fn editing_a_file_invalidates_only_that_file() {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    build_fixture(root);

    let before = audit_repository(root).expect("audit").symbols;

    // Change one file. A content-keyed cache must notice.
    write(
        root,
        "app/store.py",
        "def fetch_all():\n    return []\n\ndef insert():\n    return 1\n\
         \ndef added_after_caching():\n    return 2\n",
    );
    let after = audit_repository(root).expect("audit");
    assert_eq!(
        after.symbols,
        before + 1,
        "the edit was served from a stale cache entry"
    );

    // Put it back; the original entry must still be usable and still correct.
    write(
        root,
        "app/store.py",
        "def fetch_all():\n    return []\n\ndef insert():\n    return 1\n",
    );
    let restored = audit_repository(root).expect("audit").symbols;
    assert_eq!(
        restored, before,
        "reverting an edit did not return to the original answer"
    );
}

#[test]
fn a_corrupt_cache_does_not_fail_the_audit() {
    // A cache row this version cannot parse must be a miss, never an error.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    build_fixture(root);
    let expected = audit_repository(root).expect("audit").symbols;

    let cache = root.join(".codereader").join("cache.sqlite3");
    fs::write(&cache, b"this is not a database").expect("clobber the cache");

    let after = audit_repository(root).expect("an unreadable cache must not fail the audit");
    assert_eq!(after.symbols, expected, "results changed after cache damage");
}
