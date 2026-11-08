//! The one error this tool must not make, in every language.
//!
//! Calling live code dead gets the report closed and never reopened. Every
//! language has ways of reaching a function that a call graph cannot follow:
//! callbacks handed to a framework, dispatch through an interface, decorators,
//! re-exports, reflection, macros. None of those are evidence of death.
//!
//! Each case below reaches a function by a route the graph cannot trace, and
//! asserts only that the function is not *accused*. Being reported as
//! unresolved is a correct answer — the tool is allowed to say it cannot tell.

use std::fs;
use std::path::Path;

use codereader::audit::{audit_repository, Classification};
use tempfile::tempdir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory");
    }
    fs::write(path, contents).expect("write fixture file");
}

/// Build a one-file repository and return the names it called dead.
fn accused(file: &str, source: &str) -> Vec<String> {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    write(root, file, source);
    let report = audit_repository(root).expect("audit runs");
    report
        .coverage
        .unaccounted
        .iter()
        .filter(|finding| finding.classification == Classification::Dead)
        .map(|finding| finding.name.clone())
        .collect()
}

/// Assert `name` was not called dead, printing everything that was.
fn not_accused(file: &str, source: &str, name: &str) {
    let dead = accused(file, source);
    assert!(
        !dead.iter().any(|found| found == name),
        "{file}: `{name}` is reached indirectly but was called dead. \
         Accused: {dead:?}"
    );
}

#[test]
fn python_a_decorated_function_is_not_dead() {
    not_accused(
        "app.py",
        "import celery\n\n\
         @celery.task\n\
         def send_email(to):\n    return to\n",
        "send_email",
    );
}

#[test]
fn python_a_name_in_dunder_all_is_not_dead() {
    not_accused(
        "lib.py",
        "__all__ = [\"public_helper\"]\n\n\
         def public_helper():\n    return 1\n",
        "public_helper",
    );
}

#[test]
fn python_a_function_passed_as_a_value_is_not_dead() {
    not_accused(
        "app.py",
        "def on_click():\n    return 1\n\n\
         def setup(widget):\n    widget.bind(on_click)\n",
        "on_click",
    );
}

#[test]
fn javascript_a_callback_handed_to_a_framework_is_not_dead() {
    not_accused(
        "app.js",
        "function handleResize() { return 1; }\n\
         window.addEventListener('resize', handleResize);\n",
        "handleResize",
    );
}

#[test]
fn javascript_a_re_export_is_not_dead() {
    not_accused(
        "index.js",
        "export function publicThing() { return 1; }\n",
        "publicThing",
    );
}

#[test]
fn typescript_an_interface_method_is_not_dead() {
    not_accused(
        "svc.ts",
        "interface Store { save(x: string): void; }\n\
         export class DbStore implements Store {\n\
           save(x: string): void { console.log(x); }\n\
         }\n",
        "save",
    );
}

#[test]
fn rust_a_trait_impl_method_is_not_dead() {
    not_accused(
        "lib.rs",
        "pub trait Render { fn render(&self) -> String; }\n\
         pub struct Page;\n\
         impl Render for Page {\n    fn render(&self) -> String { String::new() }\n}\n",
        "render",
    );
}

#[test]
fn rust_a_pub_item_in_a_library_is_not_dead() {
    not_accused(
        "lib.rs",
        "pub fn exported_api() -> u32 { helper() }\n\
         fn helper() -> u32 { 1 }\n",
        "exported_api",
    );
}

#[test]
fn go_an_interface_implementation_is_not_dead() {
    not_accused(
        "store.go",
        "package store\n\n\
         type Store interface{ Save(s string) }\n\n\
         type DB struct{}\n\n\
         func (d DB) Save(s string) {}\n",
        "Save",
    );
}

#[test]
fn java_an_override_is_not_dead() {
    not_accused(
        "Handler.java",
        "public class Handler implements Runnable {\n\
           @Override\n\
           public void run() { work(); }\n\
           private void work() {}\n\
         }\n",
        "run",
    );
}

#[test]
fn csharp_a_public_method_is_not_dead() {
    not_accused(
        "Service.cs",
        "public class Service {\n\
           public void Process() { Helper(); }\n\
           private void Helper() {}\n\
         }\n",
        "Process",
    );
}

#[test]
fn ruby_a_public_method_is_not_dead() {
    not_accused(
        "service.rb",
        "class Service\n  def process\n    helper\n  end\n\n\
         private\n\n  def helper\n    1\n  end\nend\n",
        "process",
    );
}

#[test]
fn php_a_public_method_is_not_dead() {
    not_accused(
        "Service.php",
        "<?php\nclass Service {\n\
           public function process() { return $this->helper(); }\n\
           private function helper() { return 1; }\n\
         }\n",
        "process",
    );
}

#[test]
fn no_language_accuses_anything_in_an_empty_or_broken_file() {
    // Parse failures must degrade to silence, never to accusation.
    for (file, source) in [
        ("a.py", "def broken(:\n"),
        ("a.js", "function ( { @@@"),
        ("a.rs", "fn ( { unclosed"),
        ("a.go", "package ;;;"),
        ("a.java", "class { void ("),
        ("a.ts", ""),
    ] {
        let dead = accused(file, source);
        assert!(
            dead.is_empty(),
            "{file}: unparseable source produced accusations: {dead:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The control
// ---------------------------------------------------------------------------
//
// Every rule above suppresses an accusation. Without a case proving the tool
// still accuses, they could all be passing because nothing is ever reported.

#[test]
fn a_genuinely_unreachable_function_is_still_reported() {
    let dead = accused(
        "app.py",
        "def main():\n    return used()\n\n\
         def used():\n    return 1\n\n\
         def _orphan():\n    return 2\n\n\
         if __name__ == \"__main__\":\n    main()\n",
        );
    assert!(
        dead.iter().any(|name| name == "_orphan"),
        "unreachable code must still be reported, or the suppression rules \
         above are meaningless. Accused: {dead:?}"
    );
}

#[test]
fn declaring_an_interface_does_not_excuse_an_unrelated_name() {
    // The contract rule must protect implementations of the declared method,
    // not everything that happens to sit in the same file.
    let dead = accused(
        "svc.ts",
        "interface Store { save(x: string): void; }\n\
         export class DbStore implements Store {\n\
           save(x: string): void {}\n\
         }\n\
         function _neverCalled(): void {}\n",
    );
    assert!(
        dead.iter().any(|name| name == "_neverCalled"),
        "the contract rule leaked and now excuses everything: {dead:?}"
    );
}

// ---------------------------------------------------------------------------
// Shapes that hide a call
// ---------------------------------------------------------------------------

#[test]
fn a_call_from_inside_a_closure_counts_as_a_use() {
    not_accused(
        "app.js",
        "function format(x) { return x; }\n\
         export function render(items) {\n\
           return items.map((item) => format(item));\n\
         }\n",
        "format",
    );
}

#[test]
fn a_call_from_inside_a_nested_function_counts_as_a_use() {
    not_accused(
        "app.py",
        "def helper():\n    return 1\n\n\
         def outer():\n    def inner():\n        return helper()\n    return inner\n\n\
         if __name__ == \"__main__\":\n    outer()\n",
        "helper",
    );
}

#[test]
fn a_call_awaited_in_an_async_function_counts_as_a_use() {
    not_accused(
        "app.py",
        "async def fetch_row():\n    return 1\n\n\
         async def handler():\n    return await fetch_row()\n\n\
         if __name__ == \"__main__\":\n    handler()\n",
        "fetch_row",
    );
}

#[test]
fn a_generic_function_called_with_a_type_argument_counts_as_a_use() {
    not_accused(
        "lib.rs",
        "fn convert<T: Default>() -> T { T::default() }\n\
         pub fn run() -> u32 { convert::<u32>() }\n",
        "convert",
    );
}

#[test]
fn a_call_inside_a_macro_body_counts_as_a_use() {
    // tree-sitter sees an opaque token tree here, so the call is invisible.
    // The name still appears, and that is enough to refuse to call it dead.
    not_accused(
        "lib.rs",
        "fn plural(n: u32) -> &'static str { if n == 1 { \"\" } else { \"s\" } }\n\
         pub fn report(n: u32) -> String { format!(\"{} item{}\", n, plural(n)) }\n",
        "plural",
    );
}

#[test]
fn a_method_called_through_a_variable_counts_as_a_use() {
    not_accused(
        "app.py",
        "class Service:\n    def process(self):\n        return 1\n\n\
         def run(service):\n    return service.process()\n\n\
         if __name__ == \"__main__\":\n    run(Service())\n",
        "process",
    );
}

#[test]
fn kotlin_a_called_function_is_not_dead() {
    not_accused(
        "Main.kt",
        "class UserService {\n\
         \x20   fun listUsers(): List<String> {\n\
         \x20       return fetchAll()\n\
         \x20   }\n\
         \x20   fun fetchAll(): List<String> = emptyList()\n\
         }\n\n\
         fun main() {\n\
         \x20   UserService().listUsers()\n\
         }\n",
        "fetchAll",
    );
}

#[test]
fn swift_a_called_function_is_not_dead() {
    not_accused(
        "main.swift",
        "class BookingView {\n\
         \x20   func loadBookings() -> [String] {\n\
         \x20       return fetchBookings()\n\
         \x20   }\n\
         \x20   func fetchBookings() -> [String] {\n\
         \x20       return []\n\
         \x20   }\n\
         }\n\n\
         func main() {\n\
         \x20   BookingView().loadBookings()\n\
         }\n",
        "fetchBookings",
    );
}

// ---------------------------------------------------------------------------
// Rust: reached through a trait, an attribute string, or a macro
// ---------------------------------------------------------------------------

#[test]
fn rust_a_method_of_an_external_trait_impl_is_not_dead() {
    // None of these traits is declared in the repository, and no visible code
    // calls the methods: serde_with calls `serialize_as`, the `-=` operator
    // calls `sub_assign`, bindgen calls `int_macro`. The struct itself is
    // private and unused, so nothing reaches the impl through its type either.
    let source = "use std::ops::SubAssign;\n\
         struct Size(u64);\n\
         impl SubAssign for Size {\n    fn sub_assign(&mut self, other: Self) { self.0 -= other.0; }\n}\n\
         struct Hex;\n\
         impl<'a> serde_with::SerializeAs<Wrapper<'a>> for Hex {\n\
             fn serialize_as<S>(source: &Wrapper<'a>, s: S) -> Result<S::Ok, S::Error> { todo!() }\n}\n\
         struct Callbacks;\n\
         impl bindgen::callbacks::ParseCallbacks for Callbacks {\n\
             fn int_macro(&self, name: &str, value: i64) -> Option<IntKind> { None }\n}\n";
    for name in ["sub_assign", "serialize_as", "int_macro"] {
        not_accused("src/lib.rs", source, name);
    }
}

#[test]
fn rust_a_function_named_in_an_attribute_string_is_not_dead() {
    let source = "#[derive(serde::Deserialize, serde::Serialize)]\n\
         pub struct Config {\n\
             #[serde(default = \"default_port\")]\n    port: u16,\n\
             #[serde(serialize_with = \"hex::encode_id\", skip_serializing_if = \"is_zero\")]\n    id: u64,\n\
             #[serde(with = \"humantime_serde\")]\n    timeout: u64,\n\
         }\n\
         fn default_port() -> u16 { 8545 }\n\
         fn is_zero(value: &u64) -> bool { *value == 0 }\n\
         mod hex {\n    pub(super) fn encode_id<S>(id: &u64, s: S) {}\n}\n";
    for name in ["default_port", "is_zero", "encode_id"] {
        not_accused("src/lib.rs", source, name);
    }
}

#[test]
fn rust_an_identifier_inside_a_macro_is_not_dead() {
    // Passed to a macro as a bare identifier, and named inside a
    // `macro_rules!` body. Neither is a call the grammar can see.
    let source = "macro_rules! register {\n    ($name:ident) => { fn generated() { $name(); invoked_from_rules(); } };\n}\n\
         fn invoked_from_rules() {}\n\
         fn handed_to_a_macro() {}\n\
         register!(handed_to_a_macro);\n";
    for name in ["invoked_from_rules", "handed_to_a_macro"] {
        not_accused("src/lib.rs", source, name);
    }
}

#[test]
fn the_trait_and_attribute_rules_do_not_excuse_an_unrelated_function() {
    // Control: an inherent method next to a trait impl, and a function next to
    // an attribute that names a different one, are still dead.
    let dead = accused(
        "src/lib.rs",
        "struct Size(u64);\n\
         impl Size {\n    fn zz_inherent_unused(&self) -> u64 { self.0 }\n}\n\
         impl Drop for Size {\n    fn drop(&mut self) {}\n}\n\
         #[derive(serde::Deserialize)]\n\
         pub struct Config {\n    #[serde(default = \"default_port\")]\n    port: u16,\n}\n\
         fn default_port() -> u16 { 1 }\n\
         fn zz_free_unused() -> u16 { 2 }\n",
    );
    for name in ["zz_inherent_unused", "zz_free_unused"] {
        assert!(
            dead.iter().any(|found| found == name),
            "`{name}` is unreferenced and must still be reported: {dead:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Vendored third-party code
// ---------------------------------------------------------------------------

#[test]
fn a_vendored_library_is_not_audited_as_the_projects_code() {
    // A C library bundled in a `-sys` crate. Its API is called by consumers of
    // the library, not by this repository, so none of it is this project's
    // dead code.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    write(root, "Cargo.toml", "[workspace]\nmembers = [\"foo-sys\"]\n");
    write(root, "LICENSE", "MIT License, the project\n");
    write(root, "foo-sys/Cargo.toml", "[package]\nname = \"foo-sys\"\n");
    write(root, "foo-sys/src/lib.rs", "pub fn version() -> u32 { 1 }\n");
    write(root, "foo-sys/libfoo/LICENSE", "BSD licence of the upstream\n");
    write(root, "foo-sys/libfoo/CMakeLists.txt", "project(foo C)\n");
    write(
        root,
        "foo-sys/libfoo/foo.c",
        "static int unused_upstream_helper(void) { return 2; }\nint foo_api(void) { return 1; }\n",
    );
    write(root, "third_party/zlib/inflate.c", "static int zz_inflate_helper(void) { return 0; }\n");
    let report = audit_repository(root).expect("audit runs");

    assert_eq!(
        report.vendored,
        vec!["foo-sys/libfoo".to_owned(), "third_party".to_owned()]
    );
    assert!(
        report
            .coverage
            .unaccounted
            .iter()
            .all(|finding| !finding.file.contains("libfoo") && !finding.file.contains("third_party")),
        "vendored code was audited: {:?}",
        report.coverage.unaccounted
    );
}

// ---------------------------------------------------------------------------
// Python: the `if __name__ == "__main__":` block is a startup path
// ---------------------------------------------------------------------------

#[test]
fn python_functions_called_from_the_main_block_are_accounted_for() {
    // A script: `main` and what it calls are reached from the main guard,
    // which sits outside every function.
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    write(
        root,
        "bench/run.py",
        "def helper(x):\n    return x\n\n\
         def main():\n    return helper(1)\n\n\
         def _orphan():\n    return 2\n\n\
         if __name__ == \"__main__\":\n    raise SystemExit(main())\n",
    );
    let report = audit_repository(root).expect("audit runs");
    let unaccounted: Vec<&str> = report
        .coverage
        .unaccounted
        .iter()
        .map(|finding| finding.name.as_str())
        .collect();
    assert!(!unaccounted.contains(&"main"), "{unaccounted:?}");
    assert!(!unaccounted.contains(&"helper"), "{unaccounted:?}");
    // Control: a function the block does not reach is still reported.
    assert!(unaccounted.contains(&"_orphan"), "{unaccounted:?}");
}

// ---------------------------------------------------------------------------
// C and C++
// ---------------------------------------------------------------------------
//
// Both benchmarked chess engines (Stockfish, lc0) were 9-in-10 wrong here:
// headers read as C, `Class::method` never linked to `obj.method()`, and the
// many ways C++ reaches a function without a call expression.

/// Build a multi-file repository and return the names it called dead.
fn accused_in(files: &[(&str, &str)]) -> Vec<String> {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    for (file, source) in files {
        write(root, file, source);
    }
    let report = audit_repository(root).expect("audit runs");
    report
        .coverage
        .unaccounted
        .iter()
        .filter(|finding| finding.classification == Classification::Dead)
        .map(|finding| finding.name.clone())
        .collect()
}

/// A small engine: a header declaring a class, its out-of-class definitions,
/// and a `main` that uses it through an object and a pointer.
const ENGINE_H: &str = "#pragma once\n\
    namespace chess {\n\
    class Base { public: virtual ~Base() = default; virtual int eval() const = 0; };\n\
    class Engine : public Base {\n\
     public:\n\
      Engine();\n\
      ~Engine();\n\
      void go(int depth);\n\
      void stop();\n\
      int eval() const override;\n\
      bool operator==(const Engine& other) const;\n\
      void never_called_method();\n\
    };\n\
    template <typename T> T clamp_to(T value);\n\
    }\n";

const ENGINE_CPP: &str = "#include \"engine.h\"\n\
    namespace chess {\n\
    #define CHECKED_STOP(e) (e)->stop()\n\
    Engine::Engine() {}\n\
    Engine::~Engine() {}\n\
    void Engine::go(int depth) { (void)depth; }\n\
    void Engine::stop() {}\n\
    int Engine::eval() const { return 0; }\n\
    bool Engine::operator==(const Engine& other) const { return this == &other; }\n\
    void Engine::never_called_method() {}\n\
    template <typename T> T clamp_to(T value) { return value; }\n\
    template <> int clamp_to<int>(int value) { return value < 0 ? 0 : value; }\n\
    static void on_signal(int) {}\n\
    static void (*handlers[])(int) = { on_signal };\n\
    void shutdown(Engine* engine) { CHECKED_STOP(engine); }\n\
    static int orphan_helper() { return 42; }\n\
    }\n";

const MAIN_CPP: &str = "#include \"engine.h\"\n\
    namespace chess { void shutdown(Engine* engine); }\n\
    int main() {\n\
      chess::Engine engine;\n\
      chess::Engine* ptr = &engine;\n\
      engine.go(5);\n\
      bool same = engine == *ptr;\n\
      chess::Base* base = ptr;\n\
      (void)same; (void)base->eval();\n\
      chess::shutdown(ptr);\n\
      return chess::clamp_to<int>(1);\n\
    }\n";

fn engine_accusations() -> Vec<String> {
    accused_in(&[
        ("src/engine.h", ENGINE_H),
        ("src/engine.cpp", ENGINE_CPP),
        ("src/main.cpp", MAIN_CPP),
    ])
}

#[test]
fn cpp_an_out_of_class_method_called_through_an_object_is_not_dead() {
    let dead = engine_accusations();
    assert!(!dead.iter().any(|name| name == "go"), "Engine::go called as engine.go(): {dead:?}");
    assert!(
        !dead.iter().any(|name| name.contains("::")),
        "symbol names must be bare, not `Class::method`: {dead:?}"
    );
}

#[test]
fn cpp_constructors_destructors_and_operators_are_not_dead() {
    let dead = engine_accusations();
    for implicit in ["Engine", "~Engine", "operator=="] {
        assert!(!dead.iter().any(|name| name == implicit), "{implicit} accused: {dead:?}");
    }
}

#[test]
fn cpp_a_virtual_override_is_not_dead() {
    let dead = engine_accusations();
    assert!(!dead.iter().any(|name| name == "eval"), "override accused: {dead:?}");
}

#[test]
fn cpp_a_template_specialisation_is_not_dead() {
    let dead = engine_accusations();
    assert!(!dead.iter().any(|name| name == "clamp_to"), "{dead:?}");
}

#[test]
fn cpp_a_call_inside_a_macro_body_is_not_dead() {
    let dead = engine_accusations();
    assert!(!dead.iter().any(|name| name == "stop"), "called via #define: {dead:?}");
}

#[test]
fn cpp_a_function_in_a_dispatch_table_is_not_dead() {
    let dead = engine_accusations();
    assert!(!dead.iter().any(|name| name == "on_signal"), "{dead:?}");
}

#[test]
fn cpp_genuinely_unreferenced_functions_are_still_reported() {
    // The control: every rule above suppresses an accusation, so a function
    // and a method nothing names must still be called dead.
    let dead = engine_accusations();
    for orphan in ["orphan_helper", "never_called_method"] {
        assert!(
            dead.iter().any(|name| name == orphan),
            "{orphan} is unreferenced and must still be reported: {dead:?}"
        );
    }
}

#[test]
fn cpp_a_method_defined_in_a_header_class_body_is_parsed_as_cpp() {
    // Read as C, `class` is a syntax error and the inline method vanishes or
    // comes back mangled.
    let dead = accused_in(&[
        (
            "src/position.h",
            "class Position {\n public:\n  int key() const { return k; }\n  int unused_inline() const { return 0; }\n private:\n  int k;\n};\n",
        ),
        (
            "src/main.cpp",
            "#include \"position.h\"\nint main() { Position p; return p.key(); }\n",
        ),
    ]);
    assert!(!dead.iter().any(|name| name == "key"), "{dead:?}");
    assert!(dead.iter().any(|name| name == "unused_inline"), "control: {dead:?}");
}

#[test]
fn cpp_a_function_called_only_from_cuda_is_not_dead() {
    // `.cu` files have no grammar here, but what they call is still called.
    not_accused_in(
        &[
            ("src/kernels.cu", "__global__ void k() {}\nvoid launch() { ReportCudaError(0); k<<<1, 1>>>(); }\n"),
            ("src/errors.cc", "void ReportCudaError(int status) { (void)status; }\nint main() { return 0; }\n"),
        ],
        "ReportCudaError",
    );
}

#[test]
fn cpp_a_member_skipped_by_parser_recovery_is_not_invented() {
    // `void unlock() RELEASE() { ... }`: an unexpanded annotation macro makes
    // the parser recover with a function called `RELEASE`. That is not a
    // function, and must not be accused.
    let dead = accused_in(&[(
        "src/mutex.h",
        "class Mutex {\n public:\n  void lock() ACQUIRE() { m.lock(); }\n  void unlock() RELEASE() { m.unlock(); }\n};\n",
    ), ("src/main.cpp", "int main() { return 0; }\n")]);
    for macro_name in ["ACQUIRE", "RELEASE"] {
        assert!(!dead.iter().any(|name| name == macro_name), "{dead:?}");
    }
}

#[test]
fn shell_a_function_named_only_in_a_heredoc_table_is_not_dead() {
    // Stockfish's arch detection reads `arch|predicate|args` rows from a
    // heredoc and runs `$pred $args`.
    not_accused(
        "scripts/detect.sh",
        "#!/bin/sh\n\
         match_true() { return 0; }\n\
         select_arch() {\n  while IFS='|' read -r arch pred args; do\n    $pred $args && echo \"$arch\"\n  done\n}\n\
         select_arch <<'EOF'\nx86-64|match_true|\nEOF\n",
        "match_true",
    );
}

fn not_accused_in(files: &[(&str, &str)], name: &str) {
    let dead = accused_in(files);
    assert!(
        !dead.iter().any(|found| found == name),
        "`{name}` is reached indirectly but was called dead. Accused: {dead:?}"
    );
}

// ---------------------------------------------------------------------------
// Rust restricted visibility: crate-internal, so unused means dead
// ---------------------------------------------------------------------------

/// Audit one Rust file; return (dead names, exported-but-unused names).
fn rust_dead_and_exported(source: &str) -> (Vec<String>, Vec<String>) {
    let directory = tempdir().expect("temp dir");
    let root = directory.path();
    write(root, "src/lib.rs", source);
    let report = audit_repository(root).expect("audit runs");
    let dead = report
        .coverage
        .unaccounted
        .iter()
        .filter(|finding| finding.classification == Classification::Dead)
        .map(|finding| finding.name.clone())
        .collect();
    let exported = report
        .coverage
        .public_unused
        .iter()
        .map(|finding| finding.name.clone())
        .collect();
    (dead, exported)
}

const RESTRICTED_FORMS: &[&str] = &["pub(crate)", "pub(super)", "pub(self)", "pub(in crate::inner)"];

#[test]
fn an_unused_restricted_visibility_function_is_dead() {
    // `pub(crate)` and friends can only be called inside the crate. If nothing
    // in the repository calls or names one, it is provably dead, not API.
    for form in RESTRICTED_FORMS {
        let (dead, exported) = rust_dead_and_exported(&format!(
            "pub fn api() -> u32 {{ 1 }}\n\
             pub mod inner {{\n    {form} fn zz_restricted_unused() -> u32 {{ 2 }}\n}}\n"
        ));
        assert!(
            dead.iter().any(|name| name == "zz_restricted_unused"),
            "{form}: an unused crate-internal function must be dead. dead={dead:?} exported={exported:?}"
        );
        assert!(!exported.iter().any(|name| name == "zz_restricted_unused"), "{form}");
    }
}

#[test]
fn a_used_restricted_visibility_function_is_not_dead() {
    for form in RESTRICTED_FORMS {
        let (dead, exported) = rust_dead_and_exported(&format!(
            "pub fn api() -> u32 {{ inner::zz_restricted_used() }}\n\
             pub mod inner {{\n    {form} fn zz_restricted_used() -> u32 {{ 2 }}\n}}\n"
        ));
        assert!(!dead.iter().any(|name| name == "zz_restricted_used"), "{form}: {dead:?}");
        assert!(!exported.iter().any(|name| name == "zz_restricted_used"), "{form}: {exported:?}");
    }
}

#[test]
fn a_plain_pub_function_is_still_exported_api() {
    // Control: an unrestricted `pub` is the crate's surface. Unused inside the
    // repository it is "exported, never called", never dead.
    let (dead, exported) =
        rust_dead_and_exported("pub fn zz_plain_pub_unused() -> u32 { 1 }\n");
    assert!(!dead.iter().any(|name| name == "zz_plain_pub_unused"), "{dead:?}");
    assert!(exported.iter().any(|name| name == "zz_plain_pub_unused"), "{exported:?}");
}
