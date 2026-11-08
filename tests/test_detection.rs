//! Test detection, per language.
//!
//! Every language marks test code differently, and each marker here is a rule
//! the tooling enforces rather than a habit: JUnit only runs `@Test`, xUnit only
//! runs `[Fact]`, Go only accepts `*testing.T`, Rust only compiles `#[cfg(test)]`
//! under `cargo test`.
//!
//! Matching a habit instead — a module someone happens to call `tests` — breaks
//! the moment they call it something else, which is exactly what happened.

use codereader::audit::{audit_repository, is_test_path};
use std::fs;
use tempfile::tempdir;

/// Write one source file and report how many symbols were classified as test.
fn test_symbol_count(filename: &str, source: &str) -> usize {
    let directory = tempdir().expect("temp dir");
    let path = directory.path().join(filename);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("dirs");
    }
    fs::write(&path, source).expect("write");
    let report = audit_repository(directory.path()).expect("audit");
    report.coverage.counts.test
}

#[test]
fn rust_cfg_test_whatever_the_module_is_called() {
    let found = test_symbol_count(
        "lib.rs",
        "#[cfg(test)]\nmod zqx_whatever {\n    fn helper() -> u8 { 1 }\n}\n",
    );
    assert!(found > 0, "cfg(test) block should be test code");
}

#[test]
fn java_junit_annotation() {
    let found = test_symbol_count(
        "Thing.java",
        "public class Thing {\n  @Test\n  public void checksSomething() {}\n}\n",
    );
    assert!(found > 0, "@Test should mark a method as test code");
}

#[test]
fn csharp_xunit_and_nunit_attributes() {
    for attribute in ["[Fact]", "[Test]", "[TestMethod]", "[Theory]"] {
        let source =
            format!("public class Thing {{\n  {attribute}\n  public void Checks() {{}}\n}}\n");
        let found = test_symbol_count("Thing.cs", &source);
        assert!(found > 0, "{attribute} should mark a method as test code");
    }
}

#[test]
fn go_declares_a_test_in_its_signature() {
    // Go has no annotation. The parameter type is the marker, and the compiler
    // enforces it.
    let found = test_symbol_count(
        "thing.go",
        "package main\n\nfunc TestThing(t *testing.T) {\n\tt.Fail()\n}\n",
    );
    assert!(found > 0, "*testing.T is how Go declares a test");
}

#[test]
fn cpp_googletest_macro() {
    let found = test_symbol_count(
        "thing.cc",
        "TEST(SuiteName, ChecksSomething) {\n  EXPECT_EQ(1, 1);\n}\n",
    );
    assert!(found > 0, "a TEST() macro declares a googletest case");
}

#[test]
fn python_pytest_naming_is_the_runners_rule() {
    let found = test_symbol_count(
        "test_thing.py",
        "def test_thing():\n    assert True\n",
    );
    assert!(found > 0, "pytest only collects test_* functions");
}

#[test]
fn build_tool_layouts_are_recognised() {
    // Maven, Gradle, RSpec, and testthat all enforce where tests live.
    assert!(is_test_path("src/test/java/com/acme/ThingTest.java"));
    assert!(is_test_path("app/src/test/kotlin/ThingTest.kt"));
    assert!(is_test_path("tests/testthat/test-thing.R"));
    assert!(is_test_path("spec/models/user_spec.rb"));
    assert!(is_test_path("tests/ThingTest.php"));
    assert!(is_test_path("src/thing_test.cc"));
}

#[test]
fn production_paths_are_never_mistaken_for_tests() {
    assert!(!is_test_path("src/latest/index.ts"));
    assert!(!is_test_path("src/contest/rules.py"));
    assert!(!is_test_path("app/protest.go"));
    assert!(!is_test_path("src/main/java/com/acme/Thing.java"));
}

// ---------------------------------------------------------------------------
// Adversarial naming
// ---------------------------------------------------------------------------
//
// The rule must hold when the names are actively unhelpful. Each case below
// puts the marker in a file named nothing like a test, and puts a test-sounding
// name on code that is not a test.

#[test]
fn every_marker_survives_a_file_named_nothing_like_a_test() {
    let cases = [
        (
            "rust",
            "zqx.rs",
            "#[cfg(test)]\nmod m {\n    #[test]\n    fn t() {}\n}\n",
        ),
        (
            "java",
            "Zqx.java",
            "class Zqx {\n  @Test\n  public void t() {}\n}\n",
        ),
        (
            "csharp",
            "Zqx.cs",
            "public class Zqx {\n  [Fact]\n  public void T() {}\n}\n",
        ),
        (
            "cpp",
            "zqx.cpp",
            "TEST(Suite, Case) {\n  ASSERT_EQ(1, 1);\n}\n",
        ),
    ];
    for (language, filename, source) in cases {
        let found = test_symbol_count(filename, source);
        assert!(
            found > 0,
            "{language}: the marker is the rule, the filename is not — \
             `{filename}` should still be test code"
        );
    }
}

#[test]
fn a_test_sounding_name_on_production_code_is_not_a_test() {
    // `test_mode` ships to users. Reading it as a test would remove real code
    // from the audit and inflate coverage.
    let found = test_symbol_count(
        "app.rs",
        "pub fn test_mode_enabled() -> bool { false }\n\
         pub struct TestHarnessConfig { pub retries: u8 }\n",
    );
    assert_eq!(
        found, 0,
        "a name containing `test` is a habit, not a rule — this ships"
    );
}

#[test]
fn a_go_function_named_test_without_the_signature_is_not_a_test() {
    // Go only runs `func TestX(t *testing.T)`. The name alone does nothing.
    let found = test_symbol_count(
        "helper.go",
        "package app\n\nfunc TestConnection(host string) bool {\n\treturn true\n}\n",
    );
    assert_eq!(
        found, 0,
        "Go requires the *testing.T parameter, not the name"
    );
}

#[test]
fn a_fixture_helper_in_a_test_directory_is_test_code() {
    // `build_user` is not itself collected by pytest, but it exists only to
    // support tests and never ships. Counting it as production code would put
    // a fixture into the audit as untested application behaviour — a false
    // accusation in the other direction.
    let found = test_symbol_count(
        "tests/test_utils.py",
        "def build_user(name):\n    return {\"name\": name}\n",
    );
    assert!(found > 0, "test-support code is not shipped behaviour");
}

// ---------------------------------------------------------------------------
// Tests that cannot fail
// ---------------------------------------------------------------------------

/// Audit one Rust file and return the tests reported as unable to fail.
fn rust_tests_that_cannot_fail(source: &str) -> (usize, Vec<String>) {
    let directory = tempdir().expect("temp dir");
    let path = directory.path().join("src/lib.rs");
    fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    fs::write(&path, source).expect("write");
    let report = audit_repository(directory.path()).expect("audit");
    (
        report.tests.test_functions,
        report
            .tests
            .assertion_free_tests
            .iter()
            .map(|test| test.name.clone())
            .collect(),
    )
}

#[test]
fn a_rust_test_fails_through_question_mark_unwrap_helpers_and_should_panic() {
    let (count, flagged) = rust_tests_that_cannot_fail(
        "fn value() -> Result<u8, ()> { Ok(1) }\n\
         fn compare(a: u8, b: u8) { assert_eq!(a, b); }\n\
         fn roundtrip(x: u8) { compare(x, x); }\n\
         fn boom() { let v: Vec<u8> = Vec::with_capacity(0); }\n\
         fn local_value() -> u8 { 1 }\n\
         #[cfg(test)]\n\
         mod tests {\n\
             use super::*;\n\
             #[test]\n    fn propagates() -> Result<(), ()> { value()?; Ok(()) }\n\
             #[test]\n    fn unwraps() { value().unwrap(); }\n\
             #[test]\n    fn expects() { value().expect(\"value\"); }\n\
             #[tokio::test]\n    async fn delegates() { roundtrip(3); }\n\
             #[test]\n    #[should_panic]\n    fn panics() { boom(); }\n\
             #[test]\n    fn calls_code_outside_the_repository() { let _ = std::fs::read(\"x\"); }\n\
             // A helper with a test-sounding name is not a test.\n\
             fn test_vector() -> u8 { local_value() }\n\
             // Control: this one reaches only visible code that cannot fail.\n\
             #[test]\n    fn checks_nothing() { let _ = local_value(); }\n\
         }\n",
    );
    assert_eq!(count, 7, "the seven #[test]-marked functions, not the helper");
    assert_eq!(flagged, vec!["checks_nothing".to_owned()]);
}

// ---------------------------------------------------------------------------
// Framework-declared tests and what counts as able to fail
// ---------------------------------------------------------------------------

/// Audit a repository and return (test function count, assertion-free names).
fn test_report(files: &[(&str, &str)]) -> (usize, Vec<String>) {
    let directory = tempdir().expect("temp dir");
    for (file, source) in files {
        let path = directory.path().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("dirs");
        }
        fs::write(&path, source).expect("write");
    }
    let report = audit_repository(directory.path()).expect("audit");
    let names = report
        .tests
        .assertion_free_tests
        .iter()
        .map(|test| test.name.clone())
        .collect();
    (report.tests.test_functions, names)
}

const ENGINE_TEST_CC: &str = "#include <gtest/gtest.h>\n\
    class EngineTest : public ::testing::Test {\n\
     protected:\n\
      ~EngineTest() override {}\n\
      void TearDown() override { EXPECT_TRUE(clean_); }\n\
      bool clean_ = true;\n\
    };\n\
    class QuietTest : public ::testing::Test {};\n\
    static void ExpectParses(const char* fen) { ASSERT_NE(fen, nullptr); }\n\
    TEST(Board, ParsesStartpos) { EXPECT_EQ(1 + 1, 2); }\n\
    TEST(Board, ParsesViaHelper) { ExpectParses(\"8/8/8/8/8/8/8/8 w - - 0 1\"); }\n\
    TEST_F(EngineTest, StopsCleanly) { clean_ = true; }\n\
    TEST_F(QuietTest, ChecksNothing) { int x = 1; (void)x; }\n\
    TEST(Board, AlsoChecksNothing) { int y = 2; (void)y; }\n\
    class ParamTest : public ::testing::TestWithParam<int> {};\n\
    TEST_P(ParamTest, IsPositive) { EXPECT_GT(GetParam(), 0); }\n";

#[test]
fn googletest_cases_are_counted_by_their_macro_not_their_name() {
    let (count, _) = test_report(&[("src/engine_test.cc", ENGINE_TEST_CC)]);
    // Six TEST/TEST_F/TEST_P cases. The fixture destructor `~EngineTest` and
    // the fixture hook are not test cases.
    assert_eq!(count, 6, "expected exactly the six declared cases");
}

#[test]
fn googletest_assertions_helpers_and_fixture_teardown_all_count() {
    let (_, free) = test_report(&[("src/engine_test.cc", ENGINE_TEST_CC)]);
    for checked in [
        "Board.ParsesStartpos",
        "Board.ParsesViaHelper",
        "EngineTest.StopsCleanly",
        "ParamTest.IsPositive",
    ] {
        assert!(!free.iter().any(|name| name == checked), "{checked} can fail: {free:?}");
    }
    // The controls: no assertion anywhere on their path.
    for unchecked in ["QuietTest.ChecksNothing", "Board.AlsoChecksNothing"] {
        assert!(free.iter().any(|name| name == unchecked), "{unchecked} checks nothing: {free:?}");
    }
    assert!(!free.iter().any(|name| name.starts_with('~')), "{free:?}");
}

#[test]
fn a_test_checked_by_an_asserting_teardown_hook_can_fail() {
    // Stockfish's instrumented tests: each sends a command and waits; the
    // class's `afterEach` asserts the engine output after every one.
    let (_, free) = test_report(&[(
        "tests/instrumented.py",
        "class TestInteractive:\n\
         \x20   def afterEach(self):\n\
         \x20       assert self.engine.ok()\n\n\
         \x20   def test_uci_command(self):\n\
         \x20       self.engine.send('uci')\n\n\
         class TestNoHook:\n\
         \x20   def test_unchecked(self):\n\
         \x20       value = 1\n",
    )]);
    assert!(!free.iter().any(|name| name == "test_uci_command"), "{free:?}");
    assert!(free.iter().any(|name| name == "test_unchecked"), "control: {free:?}");
}

#[test]
fn a_test_calling_a_test_helper_that_raises_can_fail() {
    // `engine.equals("readyok")` blocks on `readline`, which raises on timeout.
    let (_, free) = test_report(&[
        (
            "tests/testing.py",
            "class Engine:\n\
             \x20   def readline(self):\n\
             \x20       if self.dead:\n\
             \x20           raise TimeoutError('no output')\n\
             \x20       return ''\n\n\
             \x20   def equals(self, expected):\n\
             \x20       for line in self.readline():\n\
             \x20           if line == expected:\n\
             \x20               return\n",
        ),
        (
            "tests/test_engine.py",
            "from testing import Engine\n\n\
             def test_ready(engine):\n\
             \x20   engine.equals('readyok')\n\n\
             def test_callback(engine):\n\
             \x20   def check(line):\n\
             \x20       assert line\n\
             \x20   engine.watch(check)\n\n\
             def test_checks_nothing(engine):\n\
             \x20   value = 1\n",
        ),
    ]);
    assert!(!free.iter().any(|name| name == "test_ready"), "{free:?}");
    assert!(!free.iter().any(|name| name == "test_callback"), "{free:?}");
    assert!(
        free.iter().any(|name| name == "test_checks_nothing"),
        "control: a test that reaches nothing able to fail is still reported: {free:?}"
    );
}

#[test]
fn a_shell_helper_the_script_calls_is_not_a_test_case() {
    // `run_test` in Stockfish's perft.sh is called fifteen times by the script
    // itself; no runner discovers it.
    let (count, free) = test_report(&[(
        "tests/perft.sh",
        "#!/bin/sh\nrun_test() {\n  echo \"$1\"\n}\nrun_test startpos 7\nrun_test kiwipete 5\n",
    )]);
    assert_eq!(count, 0, "{free:?}");
    assert!(free.is_empty(), "{free:?}");
}
