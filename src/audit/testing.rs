//! Test quality.
//!
//! "Do you have tests?" is the wrong question — every repository has some. The
//! questions that matter are:
//!
//! 1. **Which entrypoints does no test reach?** Not "what percent of lines are
//!    covered" — line coverage weights a one-line getter the same as a payment
//!    handler. The graph knows which symbols are externally reachable, so the
//!    gap can be reported as "`POST /bookings` has no test", which is
//!    actionable in a way that "71% coverage" never is.
//!
//! 2. **Which tests assert nothing?** A test that calls code and checks no
//!    result can never fail. It produces coverage and proves nothing. These are
//!    worse than missing tests, because they read as safety.
//!
//! Both are answered from the graph, with no test execution. Running the suite
//! (and mutation testing on top of it) is strictly stronger evidence and is the
//! natural next layer, but it needs a sandbox; this does not.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::graph::{EdgeType, Graph, NodeKind};
use super::reachability::ReachabilityIndex;
use super::{is_test_path, EntryPointKind, Exposure};
use crate::index::LineRange;

/// Function names that constitute an assertion across common frameworks.
const ASSERTION_NAMES: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "assertEqual",
    "assertEquals",
    "assertNotEqual",
    "assertTrue",
    "assertFalse",
    "assertRaises",
    "assertIn",
    "assertIsNone",
    "assertIsNotNone",
    "assertThat",
    "expect",
    "should",
    "shouldBe",
    "toBe",
    "toEqual",
    "toMatch",
    "toThrow",
    "toHaveBeenCalled",
    "require",
    "check",
    "verify",
    "fail",
    "panic",
    "raises",
    "throws",
    // Rust: `unwrap`/`expect` panic on the failure case, which fails the test.
    "unwrap",
    "unwrap_err",
    "expect_err",
    // anyhow/eyre: `ensure!` and `bail!` return an error, which fails a test
    // that returns `Result`.
    "ensure",
    "bail",
];

/// Prefixes of the assertion macros C and C++ test frameworks define:
/// googletest/googlemock, Catch2/doctest, Boost.Test.
const ASSERTION_MACRO_PREFIXES: &[&str] = &[
    "EXPECT_",
    "ASSERT_",
    "GTEST_FAIL",
    "ADD_FAILURE",
    "FAIL",
    "REQUIRE",
    "CHECK",
    "BOOST_CHECK",
    "BOOST_REQUIRE",
    "BOOST_TEST",
    "BOOST_FAIL",
];

pub fn is_assertion_name(name: &str) -> bool {
    ASSERTION_NAMES.contains(&name)
        // `assert_eq`, `debug_assert`, proptest's `prop_assert_eq`.
        || name.contains("assert")
        || name.starts_with("expect")
        || name.starts_with("should")
        // Shared test helpers, often in a crate outside the repository where
        // their body cannot be followed: `test_utils::test_decode(..)`,
        // `check_roundtrip(..)`. A test that hands its result to one is
        // checking it.
        || name.starts_with("test_")
        || name.starts_with("check_")
        || name.starts_with("verify_")
        || ASSERTION_MACRO_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Hooks a test runner calls after the tests of a class or module, by the
/// name each framework gives them: unittest, pytest, Mocha/Jest-style
/// runners, googletest. An assertion in one of them checks every test it
/// runs after.
const TEARDOWN_HOOKS: &[&str] = &[
    "tearDown",
    "tearDownClass",
    "tearDownModule",
    "teardown_method",
    "teardown_class",
    "teardown_function",
    "teardown_module",
    "teardown",
    "afterEach",
    "afterAll",
    "after_each",
    "after_all",
    "TearDown",
    "TearDownTestSuite",
    "TearDownTestCase",
];

/// A test case declared by a framework construct rather than by its name:
/// googletest's `TEST(Suite, Name)`, `TEST_F(Fixture, Name)`, and the like.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredTest {
    /// `function:<file>:<name>`, the same line-less prefix as assertion
    /// evidence.
    pub id_prefix: String,
    /// The fixture class whose `SetUp`/`TearDown` run around the test.
    pub fixture: Option<String>,
}

/// Everything known about tests before the graph is built.
#[derive(Debug, Clone, Default)]
pub struct TestEvidence {
    /// Line-less ids of symbols that can fail: they assert, raise, call a
    /// test helper that does, or run under a teardown hook that does.
    pub asserting: Vec<String>,
    /// Line-less ids of framework-declared test cases.
    pub declared: Vec<String>,
    /// Line-less ids of functions their own file calls. A test runner, not
    /// the file, invokes a test; a function the file calls is a helper.
    pub invoked_in_file: Vec<String>,
}

fn symbol_path(symbol: &crate::index::Symbol) -> String {
    symbol.file.to_string_lossy().replace('\\', "/")
}

fn symbol_prefix(symbol: &crate::index::Symbol) -> String {
    super::graph::function_id_for(&symbol_path(symbol), &symbol.name)
}

fn is_test_code(symbol: &crate::index::Symbol) -> bool {
    is_test_path(&symbol_path(symbol)) || super::graph::is_test_scope(&symbol.enclosing_scope)
}

/// Expand direct assertion evidence into everything that can fail.
///
/// A test that only calls `engine.expect("readyok")` asserts nothing itself,
/// but the helper blocks with a timeout and raises; a test whose class runs
/// an asserting `afterEach` is checked after every run. Reporting either as
/// "can never fail" is a false accusation. Only helpers in test code
/// propagate: production code that raises is what a test checks, not a check.
pub fn collect_evidence(
    symbols: &[crate::index::Symbol],
    calls: &[super::graph::CallSite],
    direct: &[String],
    declared: &[DeclaredTest],
) -> TestEvidence {

    let mut asserting: HashSet<String> = direct.iter().cloned().collect();

    // 1. Teardown hooks: an asserting hook checks every symbol in its scope.
    let mut asserting_scopes: HashSet<(String, String)> = HashSet::new();
    let mut asserting_fixtures: HashSet<String> = HashSet::new();
    for symbol in symbols {
        if TEARDOWN_HOOKS.contains(&symbol.name.as_str()) && asserting.contains(&symbol_prefix(symbol))
        {
            asserting_scopes.insert((symbol_path(symbol), symbol.enclosing_scope.clone()));
            if let Some(class) = symbol.enclosing_scope.rsplit("::").next() {
                asserting_fixtures.insert(class.to_owned());
            }
        }
    }
    if !asserting_scopes.is_empty() {
        for symbol in symbols {
            if asserting_scopes.contains(&(symbol_path(symbol), symbol.enclosing_scope.clone())) {
                asserting.insert(symbol_prefix(symbol));
            }
        }
    }
    for test in declared {
        if test
            .fixture
            .as_ref()
            .is_some_and(|fixture| asserting_fixtures.contains(fixture))
        {
            asserting.insert(test.id_prefix.clone());
        }
    }

    // 2. Test helpers, by name, to a fixed point. By name rather than by
    //    resolved edge, because an unresolved call to an asserting helper
    //    still asserts. Only callers in test code are followed.
    // Only files holding test code can hold a test or a test helper, so
    // every call outside them is skipped before anything is allocated.
    let mut test_files: HashMap<String, bool> = HashMap::new();
    let mut scoped_test_code: HashSet<(String, String)> = HashSet::new();
    for symbol in symbols {
        let path = symbol_path(symbol);
        let in_test_path = *test_files
            .entry(path.clone())
            .or_insert_with(|| is_test_path(&path));
        if !in_test_path && super::graph::is_test_scope(&symbol.enclosing_scope) {
            scoped_test_code.insert((path.clone(), symbol.name.clone()));
            test_files.insert(path, true);
        }
    }
    let mut asserting_helpers: HashSet<String> = symbols
        .iter()
        .filter(|symbol| is_test_code(symbol) && asserting.contains(&symbol_prefix(symbol)))
        .map(|symbol| symbol.name.clone())
        .collect();
    let mut file_has_tests: HashMap<&str, bool> = HashMap::new();
    let mut test_calls: Vec<(String, &str, &str)> = Vec::new();
    let mut invoked_in_file: Vec<String> = Vec::new();
    for call in calls {
        let has_tests = *file_has_tests.entry(call.file.as_str()).or_insert_with(|| {
            test_files
                .get(call.file.as_str())
                .copied()
                .unwrap_or_else(|| is_test_path(&call.file))
        });
        if !has_tests {
            continue;
        }
        if looks_like_test_function(&call.callee) {
            invoked_in_file.push(super::graph::function_id_for(&call.file, &call.callee));
        }
        let Some(caller) = call.caller.as_ref() else {
            continue;
        };
        let in_test = is_test_path(&call.file)
            || scoped_test_code.contains(&(call.file.clone(), caller.clone()));
        if in_test {
            test_calls.push((
                super::graph::function_id_for(&call.file, caller),
                caller.as_str(),
                call.callee.as_str(),
            ));
        }
    }
    loop {
        let mut changed = false;
        for (caller_id, caller, callee) in &test_calls {
            if asserting_helpers.contains(*callee) && !asserting.contains(caller_id) {
                asserting.insert(caller_id.clone());
                asserting_helpers.insert((*caller).to_owned());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    invoked_in_file.sort();
    invoked_in_file.dedup();

    let mut asserting: Vec<String> = asserting.into_iter().collect();
    asserting.sort();
    let mut declared: Vec<String> = declared.iter().map(|test| test.id_prefix.clone()).collect();
    declared.sort();
    declared.dedup();
    TestEvidence {
        asserting,
        declared,
        invoked_in_file,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntestedEntrypoint {
    pub name: String,
    pub kind: EntryPointKind,
    pub route: Option<String>,
    pub file: String,
    pub line_range: LineRange,
    /// Whether a handler could be resolved at all. An entrypoint with no
    /// resolved handler is untestable *by this analysis*, not proven untested.
    pub handler_resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionFreeTest {
    pub name: String,
    pub file: String,
    pub line_range: LineRange,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TestAnalysis {
    pub test_functions: usize,
    /// Tests that check nothing and reach no code that could fail them: no
    /// assertion, `?`, `unwrap`, or panic, directly or in anything they call,
    /// and no call into code outside the repository. These cannot fail.
    pub assertion_free_tests: Vec<AssertionFreeTest>,
    pub external_entrypoints: usize,
    pub tested_entrypoints: usize,
    /// Entrypoints no test reaches — the actionable gap list.
    pub untested_entrypoints: Vec<UntestedEntrypoint>,
    /// Fraction of external entrypoints a test reaches, 0-1.
    pub entrypoint_test_coverage: f64,
}

/// Analyse test quality from the graph.
///
/// `asserting_symbols` lists the symbols proven to contain an assertion,
/// gathered during extraction (calls to assertion functions, Python `assert`
/// statements, Rust `assert!`/`panic!` macros, `?`, `unwrap`/`expect`).
///
/// `test_functions` lists node ids the language itself marks as tests (Rust's
/// `#[test]` family). In a language that has such a marker the marker decides,
/// and the name does not: a helper called `test_vector()` is not a test, and
/// `#[test] fn roundtrip()` is one.
pub fn analyse(
    graph: &Graph,
    reachability: &ReachabilityIndex,
    asserting_symbols: &[String],
    test_functions: &[String],
) -> TestAnalysis {
    analyse_with(
        graph,
        reachability,
        &TestEvidence {
            asserting: asserting_symbols.to_vec(),
            ..TestEvidence::default()
        },
        test_functions,
    )
}

/// [`analyse`], with the full evidence gathered at extraction time.
pub fn analyse_with(
    graph: &Graph,
    reachability: &ReachabilityIndex,
    evidence: &TestEvidence,
    test_functions: &[String],
) -> TestAnalysis {
    // Evidence is recorded as `function:<file>:<name>`, before the symbol's
    // line is known; a node id is that prefix plus `:<line>`.
    let line_less = |node_id: &str| -> String {
        node_id
            .rsplit_once(':')
            .map_or(node_id, |(prefix, _)| prefix)
            .to_owned()
    };
    let declared: HashSet<&str> = evidence.declared.iter().map(String::as_str).collect();
    let invoked: HashSet<&str> = evidence.invoked_in_file.iter().map(String::as_str).collect();

    let marked: HashSet<&str> = test_functions.iter().map(String::as_str).collect();
    let test_symbols: Vec<&super::graph::GraphNode> = graph
        .nodes
        .iter()
        .filter(|node| {
            if node.kind != NodeKind::Function {
                return false;
            }
            let prefix = line_less(&node.id);
            if declared.contains(prefix.as_str()) {
                return true;
            }
            if has_test_attribute(&node.language) {
                return marked.contains(node.id.as_str());
            }
            (is_test_path(&node.file) || node.in_test_scope)
                && looks_like_test_function(&node.name)
                && !invoked.contains(prefix.as_str())
        })
        .collect();

    let can_fail = failure_closure(graph, &evidence.asserting);
    let asserts = |node_id: &str| can_fail.contains(node_id);

    let mut assertion_free_tests: Vec<AssertionFreeTest> = test_symbols
        .iter()
        .filter(|node| !asserts(&node.id))
        .map(|node| AssertionFreeTest {
            name: node.name.clone(),
            file: node.file.clone(),
            line_range: node.line_range,
        })
        .collect();
    assertion_free_tests.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line_range.start.cmp(&right.line_range.start))
    });

    // An entrypoint is tested when a test reaches its handler.
    let mut untested = Vec::new();
    let mut tested = 0usize;
    let mut external = 0usize;

    // The first `HandledBy` edge out of each node, in edge order. Built once so
    // the loop below does not rescan every edge per entrypoint.
    let mut handler_by_source: HashMap<&str, &str> = HashMap::new();
    for edge in graph
        .edges
        .iter()
        .filter(|edge| edge.edge_type == EdgeType::HandledBy)
    {
        handler_by_source
            .entry(edge.source.as_str())
            .or_insert(edge.target.as_str());
    }

    // The externally reachable surface: routes for a service, exported
    // functions for a library. Measuring routes alone printed "0 of 0" on every
    // package, which tells a reader nothing about whether it is tested.
    for node in graph.nodes.iter().filter(|node| {
        !is_test_path(&node.file)
            && ((node.kind == NodeKind::EntryPoint
                && node.exposure == Some(Exposure::External))
                || (node.kind == NodeKind::Function
                    && node.is_public
                    && !node.in_test_scope))
    }) {
        external += 1;

        let handler = handler_by_source.get(node.id.as_str()).copied();

        let covered = match &handler {
            Some(handler_id) => reachability.reached_by_tests(handler_id),
            // No handler resolved: fall back to whether the entrypoint itself is
            // reached, which is rare but possible for bare registrations.
            None => reachability.reached_by_tests(&node.id),
        };

        if covered {
            tested += 1;
        } else {
            untested.push(UntestedEntrypoint {
                name: node.name.clone(),
                kind: node.entrypoint_kind.unwrap_or(EntryPointKind::Application),
                route: None,
                file: node.file.clone(),
                line_range: node.line_range,
                handler_resolved: handler.is_some(),
            });
        }
    }

    untested.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line_range.start.cmp(&right.line_range.start))
    });

    TestAnalysis {
        test_functions: test_symbols.len(),
        assertion_free_tests,
        external_entrypoints: external,
        tested_entrypoints: tested,
        entrypoint_test_coverage: if external == 0 {
            0.0
        } else {
            round2(tested as f64 / external as f64)
        },
        untested_entrypoints: untested,
    }
}

/// Languages whose tests are declared by an attribute the extractor records.
fn has_test_attribute(language: &str) -> bool {
    language == "rust"
}

/// Every function that can fail: one that asserts, or that calls code this
/// repository does not define, directly or through any function it calls.
///
/// A test that delegates to `check_roundtrip(input)` fails when that helper
/// asserts, so the evidence flows backwards along call edges — proven ones and
/// inferred dispatch alike, since following too few edges is what produces a
/// false "this test checks nothing". A call that resolves to nothing here
/// (`Command::parse_from`, a helper in another crate) may assert or panic
/// where it cannot be seen, so it counts as a way to fail too: the finding
/// claims the test *cannot* fail, and that is only provable over code that is
/// all visible.
fn failure_closure(graph: &Graph, asserting_symbols: &[String]) -> HashSet<String> {
    // Assertion evidence is recorded as `function:<file>:<name>`, before the
    // symbol's line is known; a node id is that prefix plus `:<line>`.
    let direct: HashSet<&str> = asserting_symbols.iter().map(String::as_str).collect();
    let mut asserting: HashSet<String> = graph
        .nodes
        .iter()
        .filter(|node| is_function_node(node))
        .filter(|node| {
            direct.contains(node.id.as_str())
                || node
                    .id
                    .rsplit_once(':')
                    .is_some_and(|(prefix, _)| direct.contains(prefix))
        })
        .map(|node| node.id.clone())
        .collect();
    asserting.extend(graph.opaque_callers.iter().cloned());

    let functions: HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|node| is_function_node(node))
        .map(|node| node.id.as_str())
        .collect();
    let mut callers: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if matches!(edge.edge_type, EdgeType::Calls | EdgeType::DynamicCalls) {
            callers
                .entry(edge.target.as_str())
                .or_default()
                .push(edge.source.as_str());
        }
    }
    let mut queue: VecDeque<String> = asserting.iter().cloned().collect();
    while let Some(current) = queue.pop_front() {
        let Some(sources) = callers.get(current.as_str()) else {
            continue;
        };
        for source in sources {
            // Evidence flows through functions only. A type is "called" by
            // every annotation that names it, and its trait methods are
            // dispatched from it; passing assertion credit through it would
            // credit any test that merely mentions the type.
            if !functions.contains(source) {
                continue;
            }
            if asserting.insert((*source).to_owned()) {
                queue.push_back((*source).to_owned());
            }
        }
    }
    asserting
}

fn is_function_node(node: &super::graph::GraphNode) -> bool {
    node.kind == NodeKind::Function
        && node.symbol_kind != Some(crate::index::SymbolKind::Class)
}

/// Test-runner conventions: pytest `test_*`, JS `test`/`it`/`describe` blocks,
/// Rust `#[test] fn ...`, JUnit `test*`/`should*`.
///
/// PascalCase names are excluded. A React test file routinely defines a fixture
/// component called `TestComponent`, which is scaffolding, not a test — counting
/// it produces a false "this test asserts nothing" finding.
fn looks_like_test_function(name: &str) -> bool {
    // A test is a plain identifier. `~EngineTest` is a fixture's destructor
    // and `operator()` an operator; neither is a test case.
    let plain_identifier = name
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    if !plain_identifier {
        return false;
    }
    let starts_uppercase = name
        .chars()
        .next()
        .map(|character| character.is_ascii_uppercase())
        .unwrap_or(false);
    if starts_uppercase {
        return false;
    }

    let lower = name.to_ascii_lowercase();
    lower.starts_with("test")
        || lower.starts_with("should")
        || lower.ends_with("_test")
        || lower.ends_with("test")
        || lower == "it"
        || lower == "describe"
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::graph::{Edge, GraphNode};
    use crate::audit::EntryPointKind;

    fn function(id: &str, name: &str, file: &str) -> GraphNode {
        GraphNode {
            id: id.to_owned(),
            kind: NodeKind::Function,
            name: name.to_owned(),
            file: file.to_owned(),
            line_range: LineRange { start: 1, end: 5 },
            language: "python".to_owned(),
            entrypoint_kind: None,
            exposure: None,
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        }
    }

    fn entrypoint(id: &str, name: &str) -> GraphNode {
        GraphNode {
            id: id.to_owned(),
            kind: NodeKind::EntryPoint,
            name: name.to_owned(),
            file: "routes.py".to_owned(),
            line_range: LineRange { start: 1, end: 1 },
            language: "python".to_owned(),
            entrypoint_kind: Some(EntryPointKind::Http),
            exposure: Some(Exposure::External),
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        }
    }

    fn edge(source: &str, target: &str, edge_type: EdgeType) -> Edge {
        Edge {
            source: source.to_owned(),
            target: target.to_owned(),
            edge_type,
        }
    }

    #[test]
    fn reports_the_endpoint_no_test_reaches() {
        let graph = Graph {
            nodes: vec![
                entrypoint("ep_login", "POST /login"),
                entrypoint("ep_pay", "POST /pay"),
                function("login", "login", "routes.py"),
                function("pay", "pay", "routes.py"),
                function("test_login", "test_login", "tests/test_auth.py"),
            ],
            edges: vec![
                edge("ep_login", "login", EdgeType::HandledBy),
                edge("ep_pay", "pay", EdgeType::HandledBy),
                edge("test_login", "login", EdgeType::Calls),
            ],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let analysis = analyse(&graph, &reachability, &["test_login".to_owned()], &[]);
        let _ = &analysis;

        assert_eq!(analysis.external_entrypoints, 2);
        assert_eq!(analysis.tested_entrypoints, 1);
        assert_eq!(analysis.untested_entrypoints.len(), 1);
        assert_eq!(analysis.untested_entrypoints[0].name, "POST /pay");
        assert_eq!(analysis.entrypoint_test_coverage, 0.5);
    }

    #[test]
    fn a_test_that_asserts_nothing_is_reported() {
        let graph = Graph {
            nodes: vec![
                function("test_real", "test_real", "tests/a.py"),
                function("test_fake", "test_fake", "tests/a.py"),
            ],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        // Only `test_real` was observed to contain an assertion.
        let analysis = analyse(&graph, &reachability, &["test_real".to_owned()], &[]);

        assert_eq!(analysis.test_functions, 2);
        assert_eq!(analysis.assertion_free_tests.len(), 1);
        assert_eq!(analysis.assertion_free_tests[0].name, "test_fake");
    }

    #[test]
    fn a_test_that_delegates_to_an_asserting_helper_can_fail() {
        // `test_roundtrip` asserts nothing itself, but it calls a helper that
        // calls a helper that asserts. It fails when they do.
        let graph = Graph {
            nodes: vec![
                function("test_roundtrip", "test_roundtrip", "tests/a.py"),
                function("check", "check", "tests/a.py"),
                function("check_inner", "check_inner", "tests/a.py"),
                function("test_silent", "test_silent", "tests/a.py"),
            ],
            edges: vec![
                edge("test_roundtrip", "check", EdgeType::Calls),
                edge("check", "check_inner", EdgeType::DynamicCalls),
                // Calling a non-asserting function does not make a test assert.
                edge("test_silent", "test_roundtrip_helper", EdgeType::Calls),
            ],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let analysis = analyse(&graph, &reachability, &["check_inner".to_owned()], &[]);

        let flagged: Vec<&str> = analysis
            .assertion_free_tests
            .iter()
            .map(|test| test.name.as_str())
            .collect();
        assert_eq!(flagged, vec!["test_silent"]);
    }

    #[test]
    fn a_test_reaching_code_that_cannot_be_seen_is_not_reported() {
        // `test_parse` calls a helper that calls into another crate. Whatever
        // that crate does may fail the test, so "cannot fail" is unproven.
        let graph = Graph {
            nodes: vec![
                function("test_parse", "test_parse", "tests/a.py"),
                function("helper", "helper", "tests/a.py"),
                function("test_noop", "test_noop", "tests/a.py"),
            ],
            edges: vec![edge("test_parse", "helper", EdgeType::Calls)],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: vec!["helper".to_owned()],
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let analysis = analyse(&graph, &reachability, &[], &[]);

        let flagged: Vec<&str> = analysis
            .assertion_free_tests
            .iter()
            .map(|test| test.name.as_str())
            .collect();
        assert_eq!(flagged, vec!["test_noop"]);
    }

    #[test]
    fn rust_tests_are_the_attribute_marked_functions_not_the_named_ones() {
        let mut marked = function("roundtrip", "roundtrip", "tests/a.rs");
        marked.language = "rust".to_owned();
        let mut helper = function("test_vector", "test_vector", "tests/a.rs");
        helper.language = "rust".to_owned();
        let graph = Graph {
            nodes: vec![marked, helper],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let analysis = analyse(&graph, &reachability, &[], &["roundtrip".to_owned()]);

        assert_eq!(analysis.test_functions, 1);
        assert_eq!(analysis.assertion_free_tests[0].name, "roundtrip");
    }

    #[test]
    fn non_test_helpers_in_test_files_are_not_counted_as_tests() {
        let graph = Graph {
            nodes: vec![
                function("make_user", "make_user", "tests/factories.py"),
                function("test_thing", "test_thing", "tests/a.py"),
            ],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let analysis = analyse(&graph, &reachability, &[], &[]);

        // A fixture factory is not a test, so it cannot be an assertion-free test.
        assert_eq!(analysis.test_functions, 1);
        assert_eq!(analysis.assertion_free_tests.len(), 1);
        assert_eq!(analysis.assertion_free_tests[0].name, "test_thing");
    }

    #[test]
    fn react_fixture_components_are_not_treated_as_tests() {
        // `function TestComponent()` inside a `.test.jsx` file is scaffolding
        // used to exercise a context provider — not a test case.
        let graph = Graph {
            nodes: vec![
                function(
                    "TestComponent",
                    "TestComponent",
                    "src/contexts/LanguageContext.test.jsx",
                ),
                function(
                    "test_renders",
                    "test_renders",
                    "src/contexts/LanguageContext.test.jsx",
                ),
            ],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let analysis = analyse(&graph, &reachability, &[], &[]);

        assert_eq!(analysis.test_functions, 1);
        assert!(
            analysis
                .assertion_free_tests
                .iter()
                .all(|test| test.name != "TestComponent"),
            "a fixture component must not be reported as an assertion-free test"
        );
    }

    #[test]
    fn assertion_names_cover_common_frameworks() {
        for name in ["assert_eq", "assertEquals", "expect", "shouldBe", "toThrow"] {
            assert!(is_assertion_name(name), "{name} should count as an assertion");
        }
        assert!(!is_assertion_name("calculate_total"));
        for name in ["unwrap", "expect_err", "prop_assert_eq", "ensure", "bail", "test_decode"] {
            assert!(is_assertion_name(name), "{name} fails a Rust test");
        }
        assert!(!is_assertion_name("latest_block"));
    }
}
