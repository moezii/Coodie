//! End-to-end audit tests over real repositories on disk.
//!
//! The unit tests cover each stage in isolation; these run the whole pipeline —
//! walk, parse, entrypoints, graph, reachability, coverage, verdict — and assert
//! on what a user would actually see.

use std::fs;
use std::path::Path;

use codereader::audit::{audit_repository, Classification, VerdictStatus};
use tempfile::tempdir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write fixture file");
}

#[test]
fn audits_a_fastapi_service_end_to_end() {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();

    write(
        root,
        "app/main.py",
        r#"
from fastapi import FastAPI
from app.service import list_users, create_user

app = FastAPI()

@app.get("/users")
def get_users():
    return list_users()

@app.post("/users")
def post_users():
    return create_user()

if __name__ == "__main__":
    run()
"#,
    );
    write(
        root,
        "app/service.py",
        r#"
def list_users():
    return normalize([])

def create_user():
    return normalize({})

def normalize(value):
    return value

def orphaned_helper():
    return "nothing calls me"
"#,
    );
    write(
        root,
        "tests/test_users.py",
        r#"
def test_list_users():
    assert True
"#,
    );

    let report = audit_repository(root).expect("audit succeeds");

    // Both routes are found, with their methods and paths.
    let routes: Vec<&str> = report
        .entrypoints
        .iter()
        .filter_map(|entrypoint| entrypoint.route.as_deref())
        .collect();
    assert!(routes.contains(&"/users"), "routes: {routes:?}");
    assert_eq!(
        report
            .entrypoints
            .iter()
            .filter(|entrypoint| entrypoint.http_method.is_some())
            .count(),
        2,
        "expected GET and POST /users: {:?}",
        report.entrypoints
    );

    // Reachable service code is a feature; the orphan is a finding.
    let unaccounted: Vec<&str> = report
        .coverage
        .unaccounted
        .iter()
        .map(|finding| finding.name.as_str())
        .collect();
    assert!(
        unaccounted.contains(&"orphaned_helper"),
        "unreferenced helper should be reported: {unaccounted:?}"
    );
    assert!(
        !unaccounted.contains(&"list_users"),
        "code reached from a route must not be reported: {unaccounted:?}"
    );

    assert_eq!(report.verdict.status, VerdictStatus::FindingsFound);
    assert!(
        report.verdict.confidence >= 0.5,
        "confidence should be high enough to judge: {}",
        report.verdict.confidence
    );
}

#[test]
fn a_library_is_judged_by_its_public_api_not_reported_as_unjudgeable() {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();

    // A pure library: no routes, no main. Its exported functions are how the
    // outside world reaches it, which is what an entrypoint is to a service.
    write(
        root,
        "lib/util.py",
        r#"
def helper_one():
    return 1

def helper_two():
    return 2
"#,
    );

    let report = audit_repository(root).expect("audit succeeds");

    // The public API is the surface, so this is judgeable rather than a shrug.
    assert_ne!(
        report.verdict.status,
        VerdictStatus::Inconclusive,
        "a library should be judged by its public API, not declared unreadable"
    );
    // And its API must not be reported as dead code.
    let unaccounted: Vec<&str> = report
        .coverage
        .unaccounted
        .iter()
        .map(|finding| finding.name.as_str())
        .collect();
    assert!(
        !unaccounted.contains(&"helper_one"),
        "an exported function is the way in, not dead code: {unaccounted:?}"
    );
    assert!(
        report.tests.external_entrypoints >= 2,
        "the public API should count as the reachable surface"
    );
}

#[test]
fn test_code_is_never_reported_as_a_product_defect() {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();

    write(
        root,
        "src/server.js",
        r#"
const app = express();
app.get("/ping", ping);
function ping() { return "pong"; }
"#,
    );
    write(
        root,
        "src/server.test.js",
        r#"
function makeFixture() { return {}; }
test("ping", () => { expect(ping()).toBe("pong"); });
"#,
    );

    let report = audit_repository(root).expect("audit succeeds");

    let fixture = report
        .coverage
        .unaccounted
        .iter()
        .find(|finding| finding.name == "makeFixture");
    assert!(
        fixture.is_none(),
        "test helpers are verification evidence, not unaccounted product code"
    );
    assert!(
        report.coverage.counts.test > 0,
        "test code should be classified as test"
    );
}

#[test]
fn generated_output_is_excluded_from_findings() {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();

    write(root, "src/app.js", "const app = express();\napp.get('/x', h);\nfunction h(){}\n");
    write(root, "dist/bundle.js", "function minifiedGarbage(){}\n");

    let report = audit_repository(root).expect("audit succeeds");

    assert!(
        report
            .coverage
            .unaccounted
            .iter()
            .all(|finding| !finding.file.starts_with("dist/")),
        "generated output must never be a finding"
    );
    assert!(
        report
            .coverage
            .unaccounted
            .iter()
            .all(|finding| finding.classification != Classification::Generated)
    );
}

#[test]
fn report_serializes_to_json() {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    write(root, "src/app.py", "def f():\n    return 1\n");

    let report = audit_repository(root).expect("audit succeeds");
    let json = report.to_json().expect("serializes");

    assert!(json.contains("\"coverage\""));
    assert!(json.contains("\"verdict\""));
    assert!(json.contains("\"score\""));
}
