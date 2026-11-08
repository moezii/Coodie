use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor};

use super::{ByteRange, LineRange, SourceLanguage, Symbol, SymbolKind};

const PYTHON_DEFINITIONS: &str = include_str!("queries/python/definitions.scm");
const PYTHON_CALLS: &str = include_str!("queries/python/calls.scm");
const PYTHON_IMPORTS: &str = include_str!("queries/python/imports.scm");
const BASH_DEFINITIONS: &str = include_str!("queries/bash/definitions.scm");
const BASH_CALLS: &str = include_str!("queries/bash/calls.scm");
const C_DEFINITIONS: &str = include_str!("queries/c/definitions.scm");
const C_CALLS: &str = include_str!("queries/c/calls.scm");
const CPP_DEFINITIONS: &str = include_str!("queries/cpp/definitions.scm");
const CPP_CALLS: &str = include_str!("queries/cpp/calls.scm");
const CSHARP_DEFINITIONS: &str = include_str!("queries/csharp/definitions.scm");
const CSHARP_CALLS: &str = include_str!("queries/csharp/calls.scm");
const CSS_DEFINITIONS: &str = include_str!("queries/css/definitions.scm");
const RUST_DEFINITIONS: &str = include_str!("queries/rust/definitions.scm");
const RUST_CALLS: &str = include_str!("queries/rust/calls.scm");
const GO_DEFINITIONS: &str = include_str!("queries/go/definitions.scm");
const GO_CALLS: &str = include_str!("queries/go/calls.scm");
const GO_IMPORTS: &str = include_str!("queries/go/imports.scm");
const HTML_DEFINITIONS: &str = include_str!("queries/html/definitions.scm");
const JAVA_DEFINITIONS: &str = include_str!("queries/java/definitions.scm");
const JAVA_CALLS: &str = include_str!("queries/java/calls.scm");
const JSON_DEFINITIONS: &str = include_str!("queries/json/definitions.scm");
const OCAML_DEFINITIONS: &str = include_str!("queries/ocaml/definitions.scm");
const OCAML_CALLS: &str = include_str!("queries/ocaml/calls.scm");
const PHP_DEFINITIONS: &str = include_str!("queries/php/definitions.scm");
const PHP_CALLS: &str = include_str!("queries/php/calls.scm");
const RUBY_DEFINITIONS: &str = include_str!("queries/ruby/definitions.scm");
const RUBY_CALLS: &str = include_str!("queries/ruby/calls.scm");
const R_DEFINITIONS: &str = include_str!("queries/r/definitions.scm");
const R_CALLS: &str = include_str!("queries/r/calls.scm");
const TOML_DEFINITIONS: &str = include_str!("queries/toml/definitions.scm");
const KOTLIN_DEFINITIONS: &str = include_str!("queries/kotlin/definitions.scm");
const KOTLIN_CALLS: &str = include_str!("queries/kotlin/calls.scm");
const SWIFT_DEFINITIONS: &str = include_str!("queries/swift/definitions.scm");
const SWIFT_CALLS: &str = include_str!("queries/swift/calls.scm");
const SCALA_DEFINITIONS: &str = include_str!("queries/scala/definitions.scm");
const SCALA_CALLS: &str = include_str!("queries/scala/calls.scm");
const JAVASCRIPT_DEFINITIONS: &str = include_str!("queries/javascript/definitions.scm");
const JAVASCRIPT_CALLS: &str = include_str!("queries/javascript/calls.scm");
const JAVASCRIPT_IMPORTS: &str = include_str!("queries/javascript/imports.scm");
const TYPESCRIPT_DEFINITIONS: &str = include_str!("queries/typescript/definitions.scm");
const TYPESCRIPT_CALLS: &str = include_str!("queries/typescript/calls.scm");
const TYPESCRIPT_IMPORTS: &str = include_str!("queries/typescript/imports.scm");
const TSX_DEFINITIONS: &str = include_str!("queries/tsx/definitions.scm");
const TSX_CALLS: &str = include_str!("queries/tsx/calls.scm");
const TSX_IMPORTS: &str = include_str!("queries/tsx/imports.scm");

#[derive(Debug)]
pub struct Grammar {
    language: Language,
    definitions: Query,
    calls: Option<Query>,
    imports: Option<Query>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCall {
    pub raw_callee: String,
    pub file: PathBuf,
    pub line_range: LineRange,
    pub byte_range: ByteRange,
    pub snippet: String,
}

#[derive(Debug, Clone)]
struct SymbolMatch {
    name: String,
    byte_range: ByteRange,
    line_range: LineRange,
    /// Whether this match declares something findable, as opposed to only
    /// establishing a scope for the things inside it.
    is_definition: bool,
    kind: SymbolKind,
    /// Reachable from outside its own file.
    is_public: bool,
    /// The parser only produced this definition by inserting a missing token
    /// into the declaration before it. `void unlock() RELEASE() { ... }`, with
    /// `RELEASE` an unexpanded annotation macro, comes back as a function
    /// called `RELEASE`.
    recovered: bool,
}

#[derive(Debug, Clone)]
struct ScopeEntry {
    name: String,
    byte_range: ByteRange,
}

impl Grammar {
    pub fn language(&self) -> Language {
        self.language.clone()
    }
}

pub fn grammar_for(language: &SourceLanguage) -> Option<&'static Grammar> {
    match language.id() {
        "shell" => Some(bash_grammar()),
        "c" => Some(c_grammar()),
        "cpp" => Some(cpp_grammar()),
        "csharp" => Some(csharp_grammar()),
        "css" => Some(css_grammar()),
        "python" => Some(python_grammar()),
        "rust" => Some(rust_grammar()),
        "go" => Some(go_grammar()),
        "html" => Some(html_grammar()),
        "java" => Some(java_grammar()),
        "json" => Some(json_grammar()),
        "ocaml" => Some(ocaml_grammar()),
        "php" => Some(php_grammar()),
        "r" => Some(r_grammar()),
        "ruby" => Some(ruby_grammar()),
        "toml" => Some(toml_grammar()),
        "scala" => Some(scala_grammar()),
        "kotlin" => Some(kotlin_grammar()),
        "swift" => Some(swift_grammar()),
        "javascript" => Some(javascript_grammar()),
        "typescript" => Some(typescript_grammar()),
        "tsx" => Some(tsx_grammar()),
        _ => None,
    }
}

pub fn extract_symbols(
    grammar: &Grammar,
    language: SourceLanguage,
    path: &Path,
    source: &[u8],
    root: Node<'_>,
) -> Vec<Symbol> {
    let matches = collect_symbol_matches(&grammar.definitions, &language, source, root);
    let scopes = collect_scopes(&matches);
    let mut seen = HashSet::new();
    let mut symbols = Vec::new();

    for matched in matches {
        if !matched.is_definition {
            continue;
        }
        let key = (
            matched.byte_range.start,
            matched.byte_range.end,
            matched.name.clone(),
        );
        if !seen.insert(key) {
            continue;
        }

        let mut scope = enclosing_scope(&matched.byte_range, &scopes);
        let mut name = matched.name;
        if matches!(language.id(), "c" | "cpp") {
            // A parse error inside `#if` branches can make `else if constexpr`
            // look like a function called `constexpr`. No keyword names a
            // function, so such a match is never a symbol.
            if is_c_family_keyword(&name) || matched.recovered {
                continue;
            }
            if language.id() == "cpp" && matched.kind == SymbolKind::Function {
                // `void Search::Worker::do_move(...)` defines `do_move` in the
                // scope `Search::Worker`. Keeping the qualified text as the name
                // meant `worker.do_move()` could never be matched to it.
                let (qualifiers, mut bare) = split_cpp_qualified_name(&name);
                // An unexpanded macro before the return type (`CUTLASS_DEVICE
                // MatrixCoord thread_start()`) can fold the type into the name.
                // Only an operator's name contains a space.
                if !bare.starts_with("operator") {
                    if let Some(last) = bare.split_whitespace().last() {
                        bare = last.to_owned();
                    }
                }
                if bare.is_empty() || is_c_family_keyword(&bare) {
                    continue;
                }
                name = bare;
                if !qualifiers.is_empty() {
                    let qualified = qualifiers.join("::");
                    scope = if scope == "module" {
                        qualified
                    } else {
                        format!("{scope}::{qualified}")
                    };
                }
            }
        }
        symbols.push(Symbol {
            name,
            kind: matched.kind,
            language: language.clone(),
            file: path.to_path_buf(),
            byte_range: matched.byte_range.clone(),
            line_range: matched.line_range,
            enclosing_scope: scope.clone(),
            // Python privacy is by convention and inherits: a public helper
            // inside a private class is not reachable from outside.
            is_public: matched.is_public && !scope.starts_with('_'),
        });
    }

    symbols.sort_by(|left, right| {
        left.byte_range
            .start
            .cmp(&right.byte_range.start)
            .then(left.name.cmp(&right.name))
    });
    symbols
}

pub fn extract_calls(
    grammar: &Grammar,
    path: &Path,
    source: &[u8],
    root: Node<'_>,
) -> Vec<RawCall> {
    let Some(query) = &grammar.calls else {
        return Vec::new();
    };
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut calls = Vec::new();
    let mut seen = HashSet::new();

    let mut matches = cursor.matches(query, root, source);
    while let Some(matched) = matches.next() {
        let mut call_node = None;
        let mut callee_node = None;

        for capture in matched.captures {
            match capture_name(capture_names, capture.index) {
                "call" => call_node = Some(capture.node),
                "call.callee" => callee_node = Some(capture.node),
                _ => {}
            }
        }

        let (Some(call_node), Some(callee_node)) = (call_node, callee_node) else {
            continue;
        };
        let Some(raw_callee) = node_text(callee_node, source).map(clean_callee_text) else {
            continue;
        };
        if raw_callee.is_empty() {
            continue;
        }
        let key = (
            call_node.start_byte(),
            call_node.end_byte(),
            raw_callee.clone(),
        );
        if !seen.insert(key) {
            continue;
        }

        calls.push(RawCall {
            raw_callee,
            file: path.to_path_buf(),
            line_range: line_range_for_node(call_node),
            byte_range: byte_range_for_node(call_node),
            snippet: source_slice(source, call_node.start_byte(), call_node.end_byte()),
        });
    }

    calls.sort_by(|left, right| {
        left.byte_range
            .start
            .cmp(&right.byte_range.start)
            .then(left.raw_callee.cmp(&right.raw_callee))
    });
    calls
}

pub fn extract_import_aliases(
    grammar: &Grammar,
    source: &[u8],
    root: Node<'_>,
) -> HashMap<String, String> {
    let Some(query) = &grammar.imports else {
        return HashMap::new();
    };
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut aliases = HashMap::new();

    let mut matches = cursor.matches(query, root, source);
    while let Some(matched) = matches.next() {
        let mut module = None;
        let mut symbol = None;
        let mut alias = None;

        for capture in matched.captures {
            let Some(text) = node_text(capture.node, source) else {
                continue;
            };
            match capture_name(capture_names, capture.index) {
                "import.module" => module = Some(normalize_module_text(&text)),
                "import.symbol" => symbol = Some(clean_import_name(&text)),
                "import.alias" => alias = Some(clean_import_name(&text)),
                _ => {}
            }
        }

        let Some(module) = module.filter(|value| !value.is_empty()) else {
            continue;
        };
        let module_label = module_label(&module);
        match (symbol, alias) {
            (Some(symbol), Some(alias)) if !symbol.is_empty() && !alias.is_empty() => {
                aliases.insert(alias, format!("{module_label}.{symbol}"));
            }
            (Some(symbol), None) if !symbol.is_empty() => {
                aliases.insert(symbol.clone(), format!("{module_label}.{symbol}"));
            }
            (None, Some(alias)) if !alias.is_empty() => {
                aliases.insert(alias, module_label.to_owned());
            }
            (None, None) => {
                let bound = module.split('.').next().unwrap_or(module_label);
                aliases.insert(bound.to_owned(), module_label.to_owned());
            }
            _ => {}
        }
    }

    aliases
}

fn collect_symbol_matches(
    query: &Query,
    language: &SourceLanguage,
    source: &[u8],
    root: Node<'_>,
) -> Vec<SymbolMatch> {
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = Vec::new();

    let mut query_matches = cursor.matches(query, root, source);
    while let Some(matched) = query_matches.next() {
        let mut definition_node = None;
        let mut scope_node = None;
        let mut name = None;
        let mut scope_name = None;

        for capture in matched.captures {
            match capture_name(capture_names, capture.index) {
                "definition.function" | "definition.method" => {
                    definition_node = Some(capture.node);
                }
                "scope.class" => scope_node = Some(capture.node),
                "name" => name = node_text(capture.node, source),
                "scope.name" => scope_name = node_text(capture.node, source),
                _ => {}
            }
        }

        if let (Some(node), Some(name)) = (definition_node, name) {
            // `void lock() ACQUIRE() { ... }` can also recover as one
            // definition whose declarator is the macro and whose real
            // declarator sits in an ERROR node in front of it.
            let name = name_hidden_by_error(node, source).unwrap_or(name);
            matches.push(SymbolMatch {
                name: clean_symbol_name(&name),
                byte_range: byte_range_for_node(node),
                line_range: line_range_for_node(node),
                is_definition: true,
                is_public: is_public_definition(language, node, &name, ""),
                kind: SymbolKind::Function,
                recovered: follows_missing_token(node),
            });
        }
        if let (Some(node), Some(name)) = (scope_node, scope_name) {
            // A scope capture is a findable type when the node actually declares
            // one. `class Booking` is; `impl Booking` and `mod tests` are not.
            let declares_type = super::symbols::is_type_definition_kind(node.kind());
            matches.push(SymbolMatch {
                name: clean_symbol_name(&name),
                byte_range: byte_range_for_node(node),
                line_range: line_range_for_node(node),
                is_definition: declares_type,
                is_public: is_public_definition(language, node, &name, ""),
                kind: SymbolKind::Class,
                recovered: false,
            });
        }
    }

    matches
}

/// The declarator name inside an ERROR child that precedes a definition's own
/// declarator, if the parser recovered that way.
fn name_hidden_by_error(definition: Node<'_>, source: &[u8]) -> Option<String> {
    let declarator = definition.child_by_field_name("declarator")?;
    let mut cursor = definition.walk();
    let error = definition
        .children(&mut cursor)
        .take_while(|child| child.id() != declarator.id())
        .find(|child| child.kind() == "ERROR")?;
    let mut inner = error.walk();
    let hidden = error
        .named_children(&mut inner)
        .find(|child| child.kind() == "function_declarator")?;
    node_text(hidden.child_by_field_name("declarator")?, source)
}

/// Does the node directly follow a declaration the parser had to complete by
/// inventing a missing token?
fn follows_missing_token(node: Node<'_>) -> bool {
    let Some(previous) = node.prev_sibling() else {
        return false;
    };
    let mut last = Some(previous);
    while let Some(current) = last {
        if current.is_missing() {
            return true;
        }
        last = current.child(current.child_count().saturating_sub(1));
        if current.child_count() == 0 {
            break;
        }
    }
    false
}

fn collect_scopes(matches: &[SymbolMatch]) -> Vec<ScopeEntry> {
    let mut scopes = Vec::new();
    let mut seen = HashSet::new();

    for matched in matches {
        let key = (
            matched.byte_range.start,
            matched.byte_range.end,
            matched.name.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        scopes.push(ScopeEntry {
            name: matched.name.clone(),
            byte_range: matched.byte_range.clone(),
        });
    }

    scopes
}

fn enclosing_scope(own: &ByteRange, scopes: &[ScopeEntry]) -> String {
    let mut containing = scopes
        .iter()
        .filter(|scope| {
            scope.byte_range.start < own.start
                && own.end <= scope.byte_range.end
                && scope.byte_range.end > scope.byte_range.start
        })
        .collect::<Vec<_>>();

    containing.sort_by(|left, right| {
        let left_width = left.byte_range.end - left.byte_range.start;
        let right_width = right.byte_range.end - right.byte_range.start;
        right_width
            .cmp(&left_width)
            .then(left.byte_range.start.cmp(&right.byte_range.start))
    });

    if containing.is_empty() {
        "module".to_owned()
    } else {
        containing
            .into_iter()
            .map(|scope| scope.name.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }
}

/// Split a C++ declarator name into its qualifying scopes and the bare name,
/// dropping template arguments from both.
///
/// `Tune::Entry<Tune::PostUpdate>::init_option` -> (`[Tune, Entry]`, `init_option`),
/// `swap_endian<u8>` -> (`[]`, `swap_endian`), `Option::operator!=` ->
/// (`[Option]`, `operator!=`), `Foo::~Foo` -> (`[Foo]`, `~Foo`).
pub fn split_cpp_qualified_name(raw: &str) -> (Vec<String>, String) {
    let compact: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    let mut chars = compact.chars().peekable();
    while let Some(character) = chars.next() {
        // Once inside `operator`, everything that follows is the operator's
        // own spelling (`operator<<`, `operator()`), not template syntax.
        if depth == 0 && current.trim_start().starts_with("operator") {
            current.push(character);
            continue;
        }
        match character {
            '<' => {
                depth += 1;
                current.push(character);
            }
            '>' => {
                depth = depth.saturating_sub(1);
                current.push(character);
            }
            ':' if depth == 0 && chars.peek() == Some(&':') => {
                chars.next();
                segments.push(std::mem::take(&mut current));
            }
            _ => current.push(character),
        }
    }
    segments.push(current);

    let strip_template = |segment: &str| -> String {
        let segment = segment.trim();
        if segment.starts_with("operator") {
            return segment.to_owned();
        }
        segment
            .split('<')
            .next()
            .unwrap_or(segment)
            .trim()
            .to_owned()
    };
    let name = segments.pop().map(|last| strip_template(&last)).unwrap_or_default();
    let qualifiers = segments
        .iter()
        .map(|segment| strip_template(segment))
        .filter(|segment| !segment.is_empty())
        .collect();
    (qualifiers, name)
}

/// Reserved words of C and C++ that a parse error can leave in a name slot.
pub fn is_c_family_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "else"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "default"
            | "return"
            | "break"
            | "continue"
            | "goto"
            | "sizeof"
            | "alignof"
            | "alignas"
            | "decltype"
            | "typeof"
            | "constexpr"
            | "consteval"
            | "constinit"
            | "static_assert"
            | "_Static_assert"
            | "noexcept"
            | "throw"
            | "try"
            | "catch"
            | "new"
            | "delete"
            | "typedef"
            | "using"
            | "namespace"
            | "template"
            | "typename"
            | "static"
            | "extern"
            | "inline"
            | "const"
            | "volatile"
            | "struct"
            | "union"
            | "enum"
            | "class"
            | "public"
            | "private"
            | "protected"
            | "virtual"
            | "explicit"
            | "friend"
            | "operator"
            | "this"
            | "co_await"
            | "co_return"
            | "co_yield"
            | "requires"
            | "concept"
    )
}

fn python_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_python::LANGUAGE.into(),
            PYTHON_DEFINITIONS,
            Some(PYTHON_CALLS),
            Some(PYTHON_IMPORTS),
        )
    })
}

fn bash_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_bash::LANGUAGE.into(),
            BASH_DEFINITIONS,
            Some(BASH_CALLS),
            None,
        )
    })
}

fn c_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_c::LANGUAGE.into(),
            C_DEFINITIONS,
            Some(C_CALLS),
            None,
        )
    })
}

fn cpp_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_cpp::LANGUAGE.into(),
            CPP_DEFINITIONS,
            Some(CPP_CALLS),
            None,
        )
    })
}

fn csharp_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_c_sharp::LANGUAGE.into(),
            CSHARP_DEFINITIONS,
            Some(CSHARP_CALLS),
            None,
        )
    })
}

fn css_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| build_grammar(tree_sitter_css::LANGUAGE.into(), CSS_DEFINITIONS, None, None))
}

fn rust_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_rust::LANGUAGE.into(),
            RUST_DEFINITIONS,
            Some(RUST_CALLS),
            None,
        )
    })
}

fn go_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_go::LANGUAGE.into(),
            GO_DEFINITIONS,
            Some(GO_CALLS),
            Some(GO_IMPORTS),
        )
    })
}

fn html_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR
        .get_or_init(|| build_grammar(tree_sitter_html::LANGUAGE.into(), HTML_DEFINITIONS, None, None))
}

fn java_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_java::LANGUAGE.into(),
            JAVA_DEFINITIONS,
            Some(JAVA_CALLS),
            None,
        )
    })
}

fn json_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR
        .get_or_init(|| build_grammar(tree_sitter_json::LANGUAGE.into(), JSON_DEFINITIONS, None, None))
}

fn ocaml_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_ocaml::LANGUAGE_OCAML.into(),
            OCAML_DEFINITIONS,
            Some(OCAML_CALLS),
            None,
        )
    })
}

fn php_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_php::LANGUAGE_PHP.into(),
            PHP_DEFINITIONS,
            Some(PHP_CALLS),
            None,
        )
    })
}

fn r_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_r::LANGUAGE.into(),
            R_DEFINITIONS,
            Some(R_CALLS),
            None,
        )
    })
}

fn ruby_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_ruby::LANGUAGE.into(),
            RUBY_DEFINITIONS,
            Some(RUBY_CALLS),
            None,
        )
    })
}

fn scala_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_scala::LANGUAGE.into(),
            SCALA_DEFINITIONS,
            Some(SCALA_CALLS),
            None,
        )
    })
}

fn kotlin_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_kotlin_ng::LANGUAGE.into(),
            KOTLIN_DEFINITIONS,
            Some(KOTLIN_CALLS),
            None,
        )
    })
}

fn swift_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_swift::LANGUAGE.into(),
            SWIFT_DEFINITIONS,
            Some(SWIFT_CALLS),
            None,
        )
    })
}

fn toml_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR
        .get_or_init(|| build_grammar(tree_sitter_toml_ng::LANGUAGE.into(), TOML_DEFINITIONS, None, None))
}

fn javascript_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_javascript::LANGUAGE.into(),
            JAVASCRIPT_DEFINITIONS,
            Some(JAVASCRIPT_CALLS),
            Some(JAVASCRIPT_IMPORTS),
        )
    })
}

fn typescript_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            TYPESCRIPT_DEFINITIONS,
            Some(TYPESCRIPT_CALLS),
            Some(TYPESCRIPT_IMPORTS),
        )
    })
}

fn tsx_grammar() -> &'static Grammar {
    static GRAMMAR: OnceLock<Grammar> = OnceLock::new();
    GRAMMAR.get_or_init(|| {
        build_grammar(
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            TSX_DEFINITIONS,
            Some(TSX_CALLS),
            Some(TSX_IMPORTS),
        )
    })
}

fn build_grammar(
    language: Language,
    definitions: &str,
    calls: Option<&str>,
    imports: Option<&str>,
) -> Grammar {
    // `Language` is no longer `Copy` in tree-sitter 0.25, so the queries are
    // built from a borrow before the language is moved into the struct.
    let definitions = Query::new(&language, definitions).expect("definitions query compiles");
    let calls = calls.map(|query| Query::new(&language, query).expect("calls query compiles"));
    let imports = imports.map(|query| Query::new(&language, query).expect("imports query compiles"));
    Grammar {
        language,
        definitions,
        calls,
        imports,
    }
}

fn capture_name<'a>(capture_names: &'a [&'a str], index: u32) -> &'a str {
    capture_names.get(index as usize).copied().unwrap_or("")
}

fn byte_range_for_node(node: Node<'_>) -> ByteRange {
    ByteRange {
        start: node.start_byte(),
        end: node.end_byte(),
    }
}

fn line_range_for_node(node: Node<'_>) -> LineRange {
    LineRange {
        start: node.start_position().row + 1,
        end: inclusive_end_line(node),
    }
}

fn inclusive_end_line(node: Node<'_>) -> usize {
    let start_line = node.start_position().row + 1;
    let end_position = node.end_position();
    let mut end_line = end_position.row + 1;

    if end_position.column == 0 && end_line > start_line {
        end_line -= 1;
    }

    end_line
}

fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn source_slice(source: &[u8], start: usize, end: usize) -> String {
    String::from_utf8_lossy(&source[start..end])
        .trim()
        .to_owned()
}

fn clean_symbol_name(raw: &str) -> String {
    clean_import_name(raw)
        .rsplit('.')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_owned()
}

fn clean_import_name(raw: &str) -> String {
    raw.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim()
        .to_owned()
}

fn clean_callee_text(raw: String) -> String {
    raw.replace("?.", ".")
        .split_whitespace()
        .collect::<String>()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .to_owned()
}

fn normalize_module_text(raw: &str) -> String {
    clean_import_name(raw)
}

fn module_label(module: &str) -> &str {
    module
        .rsplit('/')
        .next()
        .unwrap_or(module)
        .trim_matches('"')
        .trim_matches('\'')
}

#[cfg(test)]
mod tests {
    use super::grammar_for;
    use crate::index::SourceLanguage;

    #[test]
    fn cpp_qualified_names_split_into_scope_and_bare_name() {
        use super::split_cpp_qualified_name as split;
        let owned = |parts: &[&str]| parts.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
        assert_eq!(split("Engine::go"), (owned(&["Engine"]), "go".to_owned()));
        assert_eq!(
            split("Search::Worker::do_move"),
            (owned(&["Search", "Worker"]), "do_move".to_owned())
        );
        assert_eq!(
            split("Tune::Entry<Tune::PostUpdate>::init_option"),
            (owned(&["Tune", "Entry"]), "init_option".to_owned())
        );
        assert_eq!(split("swap_endian<u8>"), (Vec::new(), "swap_endian".to_owned()));
        assert_eq!(split("Option::operator!="), (owned(&["Option"]), "operator!=".to_owned()));
        assert_eq!(split("operator<<"), (Vec::new(), "operator<<".to_owned()));
        assert_eq!(split("Foo::~Foo"), (owned(&["Foo"]), "~Foo".to_owned()));
        assert_eq!(split("plain"), (Vec::new(), "plain".to_owned()));
    }

    #[test]
    fn cpp_out_of_class_methods_get_their_class_as_scope() {
        let source = b"namespace NS {
class Foo { public: void bar(); int baz() { return 1; } };
void Foo::bar() {}
}
";
        let language = SourceLanguage::new("cpp").unwrap();
        let grammar = grammar_for(&language).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&grammar.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let symbols = super::extract_symbols(
            grammar,
            language,
            std::path::Path::new("a.cpp"),
            source,
            tree.root_node(),
        );
        let bar = symbols.iter().find(|s| s.name == "bar").expect("bar is a symbol");
        assert_eq!(bar.enclosing_scope, "NS::Foo");
        let baz = symbols.iter().find(|s| s.name == "baz").expect("baz is a symbol");
        assert_eq!(baz.enclosing_scope, "NS::Foo");
        assert!(symbols.iter().all(|s| !s.name.contains("::")), "{symbols:?}");
    }

    #[test]
    fn all_registered_grammar_queries_compile() {
        for id in [
            "shell",
            "c",
            "cpp",
            "csharp",
            "css",
            "go",
            "html",
            "java",
            "javascript",
            "json",
            "ocaml",
            "php",
            "python",
            "r",
            "ruby",
            "rust",
            "scala",
            "kotlin",
            "swift",
            "toml",
            "tsx",
            "typescript",
        ] {
            let language = SourceLanguage::new(id).unwrap();
            assert!(grammar_for(&language).is_some(), "missing grammar for {id}");
        }
    }
}

/// Is this definition reachable from outside its own file?
///
/// Every language spells this differently, and getting it wrong in either
/// direction is costly: too strict and a library's whole API reads as dead
/// code, too loose and nothing is ever reported. The rule per language is the
/// one its own community uses.
pub fn is_public_definition(
    language: &SourceLanguage,
    node: Node<'_>,
    name: &str,
    enclosing_scope: &str,
) -> bool {
    match language.id() {
        // Convention, not syntax: a leading underscore means private, and
        // anything nested inside a private thing is private too.
        "python" => !name.starts_with('_') && !enclosing_scope.starts_with('_'),
        // Exported identifiers are capitalised. This is enforced by the compiler.
        "go" => name.chars().next().is_some_and(|c| c.is_uppercase()),
        // A bare `pub` on the item itself. `pub(crate)`, `pub(super)`,
        // `pub(self)`, `pub(in path)` and the old `crate` visibility can only
        // be used inside the crate, so they are not API: an unused one is
        // dead, not "exported, never called".
        "rust" => has_bare_pub(node),
        // An `export` anywhere up the chain to the statement.
        "javascript" | "typescript" | "tsx" => has_ancestor_kind(node, "export_statement"),
        // An explicit `public` modifier.
        "java" | "csharp" | "cpp" | "scala" | "php" => has_modifier(node, "public"),
        // Ruby methods are public unless `private` is declared, which is a
        // runtime call rather than a modifier; assume public.
        "ruby" => true,
        // For anything else, assume reachable. Over-including a root leaves
        // code unexplained at worst; under-including it accuses live code.
        _ => true,
    }
}

/// Does a Rust item carry an unrestricted `pub`? The grammar parses
/// `pub(crate)` as a `visibility_modifier` with the restriction as further
/// children, so a bare `pub` is the one whose only child is the keyword.
fn has_bare_pub(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|child| child.kind() == "visibility_modifier");
    found.is_some_and(|visibility| {
        visibility.child_count() == 1
            && visibility.child(0).is_some_and(|keyword| keyword.kind() == "pub")
    })
}

fn has_ancestor_kind(node: Node<'_>, kind: &str) -> bool {
    let mut current = node.parent();
    let mut depth = 0;
    while let Some(candidate) = current {
        if candidate.kind() == kind {
            return true;
        }
        // A definition is at most a few levels below its export statement.
        depth += 1;
        if depth > 4 {
            return false;
        }
        current = candidate.parent();
    }
    false
}

/// Does the node carry a modifier with this text?
fn has_modifier(node: Node<'_>, keyword: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == keyword {
            return true;
        }
        if kind.contains("modifier") {
            let mut inner = child.walk();
            if child
                .children(&mut inner)
                .any(|token| token.kind() == keyword)
            {
                return true;
            }
        }
    }
    false
}
