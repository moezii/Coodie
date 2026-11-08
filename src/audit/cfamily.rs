//! C and C++ uses a call graph cannot see.
//!
//! A C++ function is reached in many ways that are not a call expression: its
//! address is taken (`&Foo::bar`), it sits in a dispatch table, a macro body
//! names it (`#define REPORT(s) CudnnError(s, __FILE__)`), it overrides a
//! virtual, it is an operator, or the compiler invokes it on its own
//! (constructors, destructors, `begin`/`end` in a range-for). None of those
//! are evidence that it is dead.
//!
//! So for C-family files every identifier that appears anywhere outside a
//! declaration counts as a mention of that name. A function is reported dead
//! only when its name appears nowhere in the repository except where it is
//! declared or defined.

use tree_sitter::Node;

/// Declarator kinds whose `declarator` child is the name being declared.
const DECLARATOR_KINDS: &[&str] = &[
    "function_declarator",
    "init_declarator",
    "pointer_declarator",
    "reference_declarator",
    "array_declarator",
    "parenthesized_declarator",
    "attributed_declarator",
    "abstract_function_declarator",
];

/// Statement kinds whose `declarator` child is the name being declared.
const DECLARING_KINDS: &[&str] = &[
    "declaration",
    "field_declaration",
    "parameter_declaration",
    "optional_parameter_declaration",
    "variadic_parameter_declaration",
    "type_definition",
    "function_definition",
    "template_declaration",
];

/// Kinds whose text is never a reference.
const OPAQUE_KINDS: &[&str] = &[
    "comment",
    "string_literal",
    "raw_string_literal",
    "char_literal",
    "system_lib_string",
    "concatenated_string",
];

/// Identifier kinds that can name a function, type or member.
const IDENTIFIER_KINDS: &[&str] = &[
    "identifier",
    "field_identifier",
    "type_identifier",
    "namespace_identifier",
];

/// Names the C++ language or standard library invokes without the source
/// spelling a call: range-for calls `begin`/`end`, structured bindings call
/// `get`, `std::swap` is found by argument-dependent lookup, and a process
/// starts at `main`.
const IMPLICITLY_INVOKED: &[&str] = &[
    "main", "begin", "end", "cbegin", "cend", "rbegin", "rend", "get", "swap", "what",
];

/// Every name a C or C++ file uses outside a declaration of that name.
///
/// `declarations` receives `(name, line)` for each place a function's name is
/// declared (a prototype, a definition, an in-class method declaration), so
/// the text-level mention gate can tell those from uses.
pub fn reference_names(
    root: Node<'_>,
    source: &[u8],
    names: &mut Vec<String>,
    declarations: &mut Vec<(String, usize)>,
) {
    /// What a child needs to know about its parent.
    struct Parent {
        kind: &'static str,
        declared: bool,
        has_body: bool,
    }

    // Is this child in the position of a name being declared?
    fn child_declared(parent: &Parent, field: Option<&str>) -> bool {
        match field {
            Some("declarator") => {
                DECLARATOR_KINDS.contains(&parent.kind) || DECLARING_KINDS.contains(&parent.kind)
            }
            // `Foo::bar` being declared: `bar` is the declared name, `Foo` is
            // a use of the class.
            Some("name") => match parent.kind {
                "qualified_identifier" | "template_function" | "template_method" => parent.declared,
                "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
                    parent.has_body
                }
                "enumerator" | "namespace_definition" | "alias_declaration" | "preproc_def"
                | "preproc_function_def" | "concept_definition" | "namespace_alias_definition" => {
                    true
                }
                _ => false,
            },
            // Macro parameters are placeholders, not uses.
            _ => parent.kind == "preproc_params" || (parent.kind == "destructor_name" && parent.declared),
        }
    }

    // One cursor for the whole tree and a set of borrowed names: this runs on
    // every C/C++ file, and a cursor or a String per node dominated its cost.
    let mut found: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut parents: Vec<Parent> = Vec::new();
    let mut cursor = root.walk();
    let mut declared = false;
    loop {
        let node = cursor.node();
        let kind = node.kind();
        let leaf = if OPAQUE_KINDS.contains(&kind) {
            true
        } else if kind == "preproc_arg" {
            // A macro body is raw text to the grammar; every word in it may
            // be a call once the macro expands.
            for word in identifier_words(node_text(node, source)) {
                found.insert(word);
            }
            true
        } else if IDENTIFIER_KINDS.contains(&kind) {
            let text = node_text(node, source).trim();
            if !declared {
                if !text.is_empty() {
                    found.insert(text);
                }
            } else if !text.is_empty() && declares_a_function(parents.iter().rev().map(|parent| parent.kind)) {
                declarations.push((text.to_owned(), node.start_position().row + 1));
            }
            true
        } else {
            false
        };

        if !leaf && cursor.goto_first_child() {
            let has_body = matches!(
                kind,
                "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier"
            ) && node.child_by_field_name("body").is_some();
            parents.push(Parent {
                kind,
                declared,
                has_body,
            });
            declared = child_declared(parents.last().expect("just pushed"), cursor.field_name());
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                if let Some(parent) = parents.last() {
                    declared = child_declared(parent, cursor.field_name());
                }
                break;
            }
            if !cursor.goto_parent() {
                names.extend(found.into_iter().map(str::to_owned));
                return;
            }
            parents.pop();
        }
    }
}

/// Is a declared name, given its parents innermost first, a function's name? True when
/// the chain of declarators above it reaches a `function_declarator`, which
/// excludes parameters, variables and fields.
fn declares_a_function<'a>(parents_innermost_first: impl Iterator<Item = &'a str>) -> bool {
    for kind in parents_innermost_first {
        if kind == "function_declarator" {
            return true;
        }
        let passes_through = DECLARATOR_KINDS.contains(&kind)
            || matches!(
                kind,
                "qualified_identifier"
                    | "template_function"
                    | "template_method"
                    | "destructor_name"
                    | "operator_name"
            );
        if !passes_through {
            return false;
        }
    }
    false
}

/// Every identifier-shaped word in a span of raw text, borrowed.
fn identifier_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .filter(|word| {
            word.chars()
                .next()
                .is_some_and(|first| first.is_alphabetic() || first == '_')
        })
}

/// Names of functions in this file that are invoked by the language rather
/// than by a call the source spells out.
pub fn implicitly_invoked_names(
    language: &str,
    root: Node<'_>,
    source: &[u8],
    names: &mut Vec<String>,
) {
    if language == "cpp" {
        names.extend(IMPLICITLY_INVOKED.iter().map(|name| (*name).to_owned()));
    }
    for_each_node(root, |node| {
        let kind = node.kind();
        if OPAQUE_KINDS.contains(&kind) {
            return false;
        }
        match kind {
            // `bool operator==(...)`: invoked by `a == b`, which is not a call
            // expression. Conversion operators likewise.
            "operator_name" | "operator_cast" => {
                names.push(normalise(node_text(node, source)));
            }
            // `void bar() override;` / `virtual void bar();`: reached through
            // dispatch on a base, which may live outside this repository
            // (`std::streambuf::sync`).
            "function_declarator" if is_virtual_function(node, source) => {
                if let Some(name) = declared_name(node, source) {
                    names.push(name);
                }
            }
            // `template <> struct hash<Key>`: a specialisation is selected by
            // the compiler from the primary template, never named on its own.
            "class_specifier" | "struct_specifier" | "union_specifier" => {
                if let Some(name) = node.child_by_field_name("name") {
                    if name.kind() == "template_type" {
                        if let Some(base) = name.child_by_field_name("name") {
                            names.push(node_text(base, source).trim().to_owned());
                        }
                    }
                }
            }
            // `extern "C"` and exported symbols are an interface for callers
            // outside this repository.
            "linkage_specification" => {
                collect_defined_names(node, source, names);
                return false;
            }
            "function_definition" | "declaration" => {
                let text = node_text(node, source);
                let head = text.split('{').next().unwrap_or("");
                if head.contains("dllexport")
                    || head.contains("visibility(\"default\")")
                    || head.contains("__attribute__((constructor")
                    || head.contains("__attribute__((destructor")
                    || head.contains("EMSCRIPTEN_KEEPALIVE")
                {
                    collect_defined_names(node, source, names);
                }
            }
            _ => {}
        }
        true
    });
}

/// Visit every node depth-first with one cursor; `visit` returns whether to
/// descend into the node's children.
fn for_each_node<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>) -> bool) {
    let mut cursor = root.walk();
    loop {
        if visit(cursor.node()) && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Does a function declarator carry `override`/`final`, or sit under a
/// `virtual` declaration?
fn is_virtual_function(declarator: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = declarator.walk();
    if declarator
        .children(&mut cursor)
        .any(|child| child.kind() == "virtual_specifier")
    {
        return true;
    }
    let mut current = declarator.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "field_declaration" | "declaration" | "function_definition" => {
                let mut inner = parent.walk();
                return parent.children(&mut inner).any(|child| {
                    child.kind() == "virtual"
                        || (child.kind() == "virtual_function_specifier"
                            && node_text(child, source).contains("virtual"))
                });
            }
            kind if DECLARATOR_KINDS.contains(&kind) => current = parent.parent(),
            _ => return false,
        }
    }
    false
}

/// The bare name declared by a function declarator.
fn declared_name(declarator: Node<'_>, source: &[u8]) -> Option<String> {
    let name = declarator.child_by_field_name("declarator")?;
    let (_, bare) = crate::index::split_cpp_qualified_name(node_text(name, source));
    (!bare.is_empty()).then_some(bare)
}

fn collect_defined_names(node: Node<'_>, source: &[u8], names: &mut Vec<String>) {
    for_each_node(node, |current| {
        if current.kind() == "function_declarator" {
            if let Some(name) = declared_name(current, source) {
                names.push(name);
            }
        }
        true
    });
}

fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Words a shell script may run as commands without spelling a call.
///
/// The shell runs whatever a word expands to: `trap cleanup EXIT`, `$pred
/// $args` with `pred` read from a heredoc table, and `eval "setup_$arch"` all
/// invoke functions no call site names. So every word, and every word inside a
/// string or heredoc, is a possible reference. Only a function's own name in
/// its definition is not.
pub fn shell_words(root: Node<'_>, source: &[u8], names: &mut Vec<String>) {
    let mut found: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for_each_node(root, |node| match node.kind() {
        "comment" => false,
        "heredoc_body" | "string" | "raw_string" | "ansi_c_string" | "string_content" => {
            found.extend(identifier_words(node_text(node, source)));
            false
        }
        "word" => {
            let defines = node.parent().is_some_and(|parent| {
                parent.kind() == "function_definition"
                    && parent
                        .child_by_field_name("name")
                        .is_some_and(|name| name.id() == node.id())
            });
            if !defines {
                found.extend(identifier_words(node_text(node, source)));
            }
            false
        }
        _ => true,
    });
    names.extend(found.into_iter().map(str::to_owned));
}

/// Macros that declare a test case: googletest, Catch2/doctest, Boost.Test.
/// Each expands to a function the test runner registers and calls.
pub const TEST_MACROS: &[&str] = &[
    "TEST",
    "TEST_F",
    "TEST_P",
    "TYPED_TEST",
    "TYPED_TEST_P",
    "TEST_CASE",
    "TEST_CASE_METHOD",
    "SCENARIO",
    "BOOST_AUTO_TEST_CASE",
    "BOOST_FIXTURE_TEST_CASE",
];

/// Macros whose first argument is a fixture class the case runs inside.
const FIXTURE_TEST_MACROS: &[&str] = &["TEST_F", "TEST_P", "TYPED_TEST", "TYPED_TEST_P"];

/// Rename each `TEST(Suite, Name) { ... }` symbol to `Suite.Name` and return
/// the cases as declared tests.
///
/// The grammar reads the macro as a function called `TEST`, so every case in
/// a file had the same name, none of them looked like a test, and the one
/// symbol that did (a fixture's `~EngineTest`) was counted instead.
pub fn name_declared_tests(
    symbols: &mut [crate::index::Symbol],
    root: Node<'_>,
    source: &[u8],
    relative: &str,
) -> Vec<super::testing::DeclaredTest> {
    let mut declared = Vec::new();
    // Only a symbol already named after a test macro can be a test case, so
    // look those up rather than walking the tree.
    let candidates: Vec<(usize, usize)> = symbols
        .iter()
        .filter(|symbol| TEST_MACROS.contains(&symbol.name.as_str()))
        .map(|symbol| (symbol.byte_range.start, symbol.byte_range.end))
        .collect();
    for (start, end) in candidates {
        let Some(mut node) = root.descendant_for_byte_range(start, end) else {
            continue;
        };
        while node.kind() != "function_definition" {
            match node.parent() {
                Some(parent) => node = parent,
                None => break,
            }
        }
        {
            if let Some((macro_name, arguments)) = test_macro_call(node, source) {
                let name = if arguments.is_empty() {
                    macro_name.to_owned()
                } else {
                    arguments.join(".")
                };
                let fixture = FIXTURE_TEST_MACROS
                    .contains(&macro_name)
                    .then(|| arguments.first().cloned())
                    .flatten();
                if let Some(symbol) = symbols.iter_mut().find(|symbol| {
                    symbol.byte_range.start == node.start_byte() && symbol.name == macro_name
                }) {
                    symbol.name = name.clone();
                    declared.push(super::testing::DeclaredTest {
                        id_prefix: super::graph::function_id_for(relative, &name),
                        fixture,
                    });
                }
            }
        }
    }
    declared
}

/// `TEST(Suite, Name)` -> (`TEST`, [`Suite`, `Name`]), for a function
/// definition whose declarator is a test macro.
fn test_macro_call(definition: Node<'_>, source: &[u8]) -> Option<(&'static str, Vec<String>)> {
    let declarator = definition.child_by_field_name("declarator")?;
    if declarator.kind() != "function_declarator" {
        return None;
    }
    let name = node_text(declarator.child_by_field_name("declarator")?, source).trim();
    let macro_name = TEST_MACROS.iter().find(|candidate| **candidate == name)?;
    let parameters = declarator.child_by_field_name("parameters")?;
    let text = node_text(parameters, source);
    let inner = text.trim().trim_start_matches('(').trim_end_matches(')');
    let arguments = inner
        .split(',')
        .map(|argument| argument.trim().trim_matches('"').trim().to_owned())
        .filter(|argument| !argument.is_empty() && !argument.starts_with('['))
        .collect();
    Some((macro_name, arguments))
}

/// Languages read only for the names they mention: C-family dialects that
/// call into C and C++ but have no grammar here.
pub fn is_lexical_reference_language(language: &str) -> bool {
    matches!(language, "cuda" | "shader" | "c-include" | "objective-c")
}

/// Every identifier-shaped word in a span of raw text.
pub fn tokenize_identifiers(text: &str, names: &mut Vec<String>) {
    let unique: std::collections::HashSet<&str> = identifier_words(text).collect();
    names.extend(unique.into_iter().map(str::to_owned));
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    source
        .get(node.byte_range())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(language: &str, source: &str) -> tree_sitter::Tree {
        let language = crate::index::SourceLanguage::new(language).expect("language");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language.tree_sitter_language().expect("grammar"))
            .expect("set language");
        parser.parse(source, None).expect("parse")
    }

    fn references(source: &str) -> Vec<String> {
        let tree = parse("cpp", source);
        let mut names = Vec::new();
        reference_names(tree.root_node(), source.as_bytes(), &mut names, &mut Vec::new());
        names
    }

    #[test]
    fn declarations_are_not_references_but_every_use_is() {
        let names = references(
            "#define REPORT(s) CudnnError(s, __FILE__)\n\
             void CudnnError(int s, const char* f);\n\
             void unused_helper(int x) {}\n\
             class Foo { public: void bar(); int kept; };\n\
             void Foo::bar() { auto p = &Foo::through_pointer; table[0] = in_table; REPORT(1); }\n\
             // comment_only()\n\
             const char* s = \"string_only\";\n",
        );
        for used in ["CudnnError", "through_pointer", "in_table", "Foo", "REPORT"] {
            assert!(names.contains(&used.to_owned()), "{used} missing from {names:?}");
        }
        for declared in ["unused_helper", "bar", "kept", "comment_only", "string_only"] {
            assert!(!names.contains(&declared.to_owned()), "{declared} wrongly in {names:?}");
        }
    }

    #[test]
    fn shell_words_in_heredocs_strings_and_arguments_are_references() {
        let source = "match_true() { return 0; }
                      cleanup() { :; }
                      never_used() { :; }
                      trap cleanup EXIT
                      select <<'EOF'
x86|match_true|
EOF
                      eval \"setup_$arch\"
";
        let tree = parse("shell", source);
        let mut names = Vec::new();
        shell_words(tree.root_node(), source.as_bytes(), &mut names);
        for used in ["match_true", "cleanup", "setup_"] {
            assert!(names.contains(&used.to_owned()), "{used} missing from {names:?}");
        }
        assert!(!names.contains(&"never_used".to_owned()), "{names:?}");
    }

    #[test]
    fn operators_overrides_and_specialisations_are_implicitly_invoked() {
        let source = "struct S : Base {\n\
                        bool operator==(const S&) const;\n\
                        int sync() override { return 0; }\n\
                        virtual void hook();\n\
                        void plain();\n\
                      };\n\
                      template <> struct hash<Key> { size_t operator()(const Key&) const; };\n\
                      extern \"C\" { void exported_api(void) {} }\n";
        let tree = parse("cpp", source);
        let mut names = Vec::new();
        implicitly_invoked_names("cpp", tree.root_node(), source.as_bytes(), &mut names);
        for expected in ["operator==", "sync", "hook", "hash", "operator()", "exported_api"] {
            assert!(names.contains(&expected.to_owned()), "{expected} missing from {names:?}");
        }
        assert!(!names.contains(&"plain".to_owned()), "{names:?}");
    }
}
