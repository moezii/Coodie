//! Every grammar-backed language, exercised end to end.
//!
//! `all_registered_grammar_queries_compile` proves the queries load. It does not
//! prove they match anything — a query can compile against a grammar and still
//! capture nothing, in which case the language silently produces no symbols and
//! nobody notices until a customer audits a repository written in it.
//!
//! These tests give each language real source and assert a known symbol comes
//! back.

use codereader::index::{parse_source_symbols, SourceLanguage, SymbolKind};
use std::path::Path;

/// Parse a snippet and return the symbol names found.
fn symbols(language_id: &str, file: &str, source: &str) -> Vec<String> {
    let language = SourceLanguage::new(language_id)
        .unwrap_or_else(|| panic!("{language_id} is not a known language"));
    parse_source_symbols(language, Path::new(file), source.as_bytes())
        .unwrap_or_else(|error| panic!("{language_id} failed to parse: {error}"))
        .into_iter()
        .map(|symbol| symbol.name)
        .collect()
}

fn assert_finds(language_id: &str, file: &str, source: &str, expected: &[&str]) {
    let found = symbols(language_id, file, source);
    for name in expected {
        assert!(
            found.contains(&(*name).to_owned()),
            "{language_id}: expected to find `{name}`, got {found:?}"
        );
    }
}

#[test]
fn python() {
    assert_finds(
        "python",
        "a.py",
        "class Service:\n    def handle(self):\n        return 1\n\ndef free():\n    return 2\n",
        &["Service", "handle", "free"],
    );
}

#[test]
fn javascript() {
    assert_finds(
        "javascript",
        "a.js",
        "function one() {}\nclass Two { three() {} }\nconst four = () => {};\n",
        &["one", "Two", "three", "four"],
    );
}

#[test]
fn typescript() {
    assert_finds(
        "typescript",
        "a.ts",
        "export function verify(t: string): boolean { return true; }\nclass Api { send() {} }\n",
        &["verify", "Api", "send"],
    );
}

#[test]
fn tsx() {
    assert_finds(
        "tsx",
        "a.tsx",
        "export function Panel() { return <div />; }\nconst Row = () => <span />;\n",
        &["Panel", "Row"],
    );
}

#[test]
fn rust() {
    assert_finds(
        "rust",
        "a.rs",
        "pub struct Booking { id: u32 }\nimpl Booking { fn cancel(&self) {} }\nfn free() {}\n",
        &["Booking", "cancel", "free"],
    );
}

#[test]
fn go() {
    assert_finds(
        "go",
        "a.go",
        "package main\n\ntype Server struct{}\n\nfunc handle() {}\n\nfunc main() {}\n",
        &["handle", "main"],
    );
}

#[test]
fn java() {
    assert_finds(
        "java",
        "A.java",
        "public class Service {\n  public void handle() {}\n}\n",
        &["Service", "handle"],
    );
}

#[test]
fn csharp() {
    assert_finds(
        "csharp",
        "A.cs",
        "public class Service {\n  public void Handle() {}\n}\n",
        &["Service", "Handle"],
    );
}

#[test]
fn ruby() {
    assert_finds(
        "ruby",
        "a.rb",
        "class Service\n  def handle\n    1\n  end\nend\n",
        &["handle"],
    );
}

#[test]
fn php() {
    assert_finds(
        "php",
        "a.php",
        "<?php\nclass Service {\n  public function handle() { return 1; }\n}\n",
        &["handle"],
    );
}

#[test]
fn c() {
    assert_finds("c", "a.c", "int add(int a, int b) { return a + b; }\n", &["add"]);
}

#[test]
fn cpp() {
    assert_finds(
        "cpp",
        "a.cpp",
        "class Service {\npublic:\n  int handle() { return 1; }\n};\n",
        &["handle"],
    );
}

#[test]
fn scala() {
    assert_finds(
        "scala",
        "a.scala",
        "object Service {\n  def handle(): Int = 1\n}\n",
        &["handle"],
    );
}

#[test]
fn ocaml() {
    assert_finds("ocaml", "a.ml", "let handle x = x + 1\n", &["handle"]);
}

#[test]
fn r_language() {
    assert_finds("r", "a.R", "handle <- function(x) {\n  x + 1\n}\n", &["handle"]);
}

#[test]
fn shell() {
    assert_finds("shell", "a.sh", "handle() {\n  echo hi\n}\n", &["handle"]);
}

#[test]
fn html() {
    // HTML has no functions; the grammar is registered for structure only.
    // Parsing must succeed rather than error.
    let found = symbols("html", "a.html", "<html><body><p>hi</p></body></html>\n");
    let _ = found;
}

#[test]
fn json_and_toml_parse_without_error() {
    let _ = symbols("json", "a.json", "{\"name\": \"app\"}\n");
    let _ = symbols("toml", "a.toml", "[package]\nname = \"app\"\n");
}

#[test]
fn css_parses_without_error() {
    let _ = symbols("css", "a.css", ".button { color: red; }\n");
}

#[test]
fn a_class_is_reported_as_a_type_not_a_function() {
    let language = SourceLanguage::new("python").expect("python");
    let parsed = parse_source_symbols(
        language,
        Path::new("a.py"),
        b"class Booking:\n    def cancel(self):\n        pass\n",
    )
    .expect("parsed");

    let booking = parsed
        .iter()
        .find(|symbol| symbol.name == "Booking")
        .expect("class found");
    assert_eq!(booking.kind, SymbolKind::Class);

    let cancel = parsed
        .iter()
        .find(|symbol| symbol.name == "cancel")
        .expect("method found");
    assert_eq!(cancel.kind, SymbolKind::Function);
    assert_eq!(cancel.enclosing_scope, "Booking");
}

// ---------------------------------------------------------------------------
// Types must be findable in their own right, not only functions.
//
// A query can capture a class as a scope label and never emit it as a symbol,
// in which case "where is the Booking model defined" has no answer and an
// unused model is invisible. These assert the type itself comes back.
// ---------------------------------------------------------------------------

#[test]
fn rust_types_are_findable() {
    assert_finds(
        "rust",
        "a.rs",
        "pub struct Booking { id: u32 }\npub enum State { Open, Closed }\npub trait Store {}\n",
        &["Booking", "State", "Store"],
    );
}

#[test]
fn go_types_are_findable() {
    assert_finds(
        "go",
        "a.go",
        "package main\n\ntype Server struct{}\n\ntype Store interface{}\n",
        &["Server", "Store"],
    );
}

#[test]
fn typescript_types_are_findable() {
    assert_finds(
        "typescript",
        "a.ts",
        "export interface User { id: string }\nexport type Id = string;\nenum Role { Admin }\n",
        &["User", "Id", "Role"],
    );
}

#[test]
fn java_and_csharp_types_are_findable() {
    assert_finds(
        "java",
        "A.java",
        "public class Booking {}\npublic interface Store {}\n",
        &["Booking", "Store"],
    );
    assert_finds(
        "csharp",
        "A.cs",
        "public class Booking {}\npublic interface IStore {}\n",
        &["Booking", "IStore"],
    );
}
