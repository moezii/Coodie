//! The language-independent last check before anything is called dead.
//!
//! Each fixture reaches a function from somewhere no extractor follows: a
//! Makefile, a YAML config, a Dockerfile, a C string handed to `dlsym`, a
//! string in another language. Before the gate every one of these was reported dead. Now each
//! must be reported as *mentioned but not traced*, with the place it was
//! mentioned.
//!
//! The controls matter as much: a genuinely unused function with a unique
//! name must still be reported dead, including when its only mention is in
//! prose documentation or in vendored code, both of which are not uses.

use std::fs;
use std::path::Path;

use codereader::audit::{audit_repository, AuditReport, Classification, Reachability};
use tempfile::tempdir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write fixture file");
}

fn audit(files: &[(&str, &str)]) -> AuditReport {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    for (relative, contents) in files {
        write(root, relative, contents);
    }
    audit_repository(root).expect("audit runs")
}

fn dead(report: &AuditReport) -> Vec<String> {
    report
        .coverage
        .unaccounted
        .iter()
        .filter(|finding| finding.classification == Classification::Dead)
        .map(|finding| finding.name.clone())
        .collect()
}

/// Assert `name` is listed as mentioned-but-not-traced, first mentioned at `site`.
fn mentioned_at(report: &AuditReport, name: &str, site: &str) {
    let finding = report
        .coverage
        .unaccounted
        .iter()
        .find(|finding| finding.name == name)
        .unwrap_or_else(|| panic!("`{name}` vanished from the report: {report:#?}"));
    assert_eq!(
        finding.reachability,
        Reachability::MentionedNotTraced,
        "`{name}` is named at {site} but was classified {:?}",
        finding.reachability
    );
    assert_eq!(finding.classification, Classification::Unknown);
    assert_eq!(finding.mentioned_at.as_deref(), Some(site), "`{name}`");
}

const TOOL_C: &str = "#include <stdio.h>\n\
\n\
#define BOOT() init_subsystem_hardware()\n\
\n\
static void init_subsystem_hardware(void) {\n\
    puts(\"hw\");\n\
}\n\
\n\
static void rebuild_lookup_tables(void) {\n\
    puts(\"tables\");\n\
}\n\
\n\
static void orphan_c_never_named(void) {\n\
    puts(\"x\");\n\
}\n\
\n\
int main(void) {\n\
    return 0;\n\
}\n";

#[test]
fn a_function_named_only_in_a_makefile_is_not_dead() {
    let report = audit(&[
        ("tool.c", TOOL_C),
        ("Makefile", "tables:\n\t./tool --run=rebuild_lookup_tables\n"),
    ]);
    mentioned_at(&report, "rebuild_lookup_tables", "Makefile:2");
    assert!(dead(&report).contains(&"orphan_c_never_named".to_owned()));
}

#[test]
fn a_c_function_looked_up_by_name_in_a_string_is_not_dead() {
    // `dlsym` finds the plugin entry by its name at run time. The C/C++
    // reference pass skips string literals, so only the gate sees this use.
    let report = audit(&[(
        "loader.c",
        "#include <dlfcn.h>

void plugin_entry_point(void) {
}

static void orphan_plugin_never_named(void) {
}

int main(void) {
    void *self = dlopen(0, RTLD_NOW);
    return dlsym(self, \"plugin_entry_point\") != 0;
}
",
    )]);
    mentioned_at(&report, "plugin_entry_point", "loader.c:11");
    assert!(dead(&report).contains(&"orphan_plugin_never_named".to_owned()));
}

/// An entrypoint, so the repository reads as an application rather than a
/// library whose every function is public API.
const PY_MAIN: &str = "if __name__ == \"__main__\":\n    print(1)\n";

#[test]
fn a_function_dispatched_from_a_yaml_config_is_not_dead() {
    let report = audit(&[
        (
            "jobs.py",
            "def nightly_cleanup():\n    return 1\n\n\ndef orphan_python_never_named():\n    return 2\n",
        ),
        ("main.py", PY_MAIN),
        (
            "schedule.yml",
            "schedule:\n  - task: jobs.nightly_cleanup\n    cron: \"0 3 * * *\"\n",
        ),
    ]);
    mentioned_at(&report, "nightly_cleanup", "schedule.yml:2");
    assert_eq!(dead(&report), vec!["orphan_python_never_named".to_owned()]);
}

#[test]
fn a_function_named_only_in_a_dockerfile_is_not_dead() {
    let report = audit(&[
        (
            "main.go",
            "package main\n\nfunc main() {}\n\nfunc exportMetricsSnapshot() {}\n\nfunc orphanGoNeverNamed() {}\n",
        ),
        (
            "Dockerfile",
            "FROM golang:1.22\nCMD [\"./app\", \"-hook=exportMetricsSnapshot\"]\n",
        ),
    ]);
    mentioned_at(&report, "exportMetricsSnapshot", "Dockerfile:2");
    assert!(dead(&report).contains(&"orphanGoNeverNamed".to_owned()));
}

#[test]
fn a_function_named_in_a_string_in_another_language_is_not_dead() {
    // A browser test drives page code by evaluating a string.
    let report = audit(&[
        (
            "web/invoice.js",
            "function formatInvoiceTotal(n) {\n  return n.toFixed(2);\n}\n\nfunction orphanJsNeverNamed() {\n  return 1;\n}\n",
        ),
        (
            "e2e/check.py",
            "def run(page):\n    page.evaluate(\"formatInvoiceTotal(3)\")\n",
        ),
    ]);
    mentioned_at(&report, "formatInvoiceTotal", "e2e/check.py:2");
    assert!(dead(&report).contains(&"orphanJsNeverNamed".to_owned()));
}

#[test]
fn same_named_definitions_block_each_other() {
    // From text alone a second definition cannot be told from a use (an
    // override, a re-export, a duplicate the runtime picks): neither is accused.
    let report = audit(&[
        ("a/tools.py", "def shared_helper():
    return 1
"),
        ("b/tools.py", "def shared_helper():
    return 2
"),
        ("main.py", PY_MAIN),
    ]);
    assert!(!dead(&report).contains(&"shared_helper".to_owned()), "{:?}", dead(&report));
}

#[test]
fn a_cpp_prototype_is_a_declaration_not_a_use() {
    // C/C++ report where they declare a name, so a header prototype and an
    // unused overload set do not excuse each other.
    let report = audit(&[
        ("shapes.h", "int area(int a);
int area(int a, int b);
void orphan_shape();
"),
        (
            "shapes.cpp",
            "#include \"shapes.h\"
int area(int a) {
  return a;
}

int area(int a, int b) {
  return a * b;
}

void orphan_shape() {
}

int main() {
  return 0;
}
",
        ),
    ]);
    let dead = dead(&report);
    assert_eq!(dead.iter().filter(|name| *name == "area").count(), 2, "{dead:?}");
    assert!(dead.contains(&"orphan_shape".to_owned()), "{dead:?}");
}

#[test]
fn a_mention_in_prose_documentation_is_not_a_use() {
    let report = audit(&[
        (
            "jobs.py",
            "def orphan_python_never_named():\n    return 2\n",
        ),
        ("main.py", PY_MAIN),
        (
            "README.md",
            "# Notes\n\n`orphan_python_never_named` is kept for compatibility.\n",
        ),
        ("docs/notes.rst", "orphan_python_never_named\n"),
        ("CHANGELOG", "- removed callers of orphan_python_never_named\n"),
    ]);
    assert_eq!(dead(&report), vec!["orphan_python_never_named".to_owned()]);
}

#[test]
fn a_mention_inside_vendored_code_is_not_a_use() {
    let report = audit(&[
        ("tool.c", TOOL_C),
        ("vendor/lib/api.h", "void orphan_c_never_named(void);\n"),
    ]);
    assert!(dead(&report).contains(&"orphan_c_never_named".to_owned()));
}

#[test]
fn the_report_lists_what_was_mentioned_but_not_traced() {
    let report = audit(&[
        ("tool.c", TOOL_C),
        ("Makefile", "tables:\n\t./tool --run=rebuild_lookup_tables\n"),
    ]);
    assert_eq!(report.coverage.mentioned_not_traced, 1);
    let markdown = codereader::audit::report::markdown(&report);
    assert!(markdown.contains("Mentioned but not traced"), "{markdown}");
    assert!(markdown.contains("`Makefile:2`"), "{markdown}");
}
