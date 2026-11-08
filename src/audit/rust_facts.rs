//! Rust-specific facts the generic extractors cannot see.
//!
//! All of them come from one walk of the tree:
//!
//! - **Attribute strings.** `#[serde(default = "default_port")]` names a
//!   function the derive calls; the source never does.
//! - **Trait-impl methods.** A method in `impl Trait for Type` is called
//!   through the trait — by generics, operators, derives, or a crate outside
//!   the repository — so the absence of a visible call proves nothing.
//! - **Test functions.** Rust marks tests with an attribute (`#[test]`,
//!   `#[tokio::test]`, ...), so the attribute decides what is a test, not the
//!   name. `#[should_panic]` is itself the check: the test fails if the body
//!   does not panic.

use tree_sitter::Node;

/// Keys whose string value configures something other than code.
const NON_CODE_KEYS: &[&str] = &[
    "feature",
    "target_os",
    "target_arch",
    "target_family",
    "target_env",
    "target_vendor",
    "target_endian",
    "target_pointer_width",
    "doc",
    // Messages, not paths: `#[should_panic(expected = "...")]`,
    // `#[deprecated(note = "...")]`.
    "expected",
    "note",
    "since",
    "reason",
    "message",
];

#[derive(Debug, Default)]
pub struct RustFacts {
    /// A path named in an attribute string, with the byte range of the item
    /// the attribute decorates.
    pub attribute_paths: Vec<(String, usize, usize)>,
    /// Each method in an `impl Trait for Type` block: the implementing type's
    /// name (when it is a plain named type) and the method's byte range.
    pub trait_impl_methods: Vec<(Option<String>, usize, usize)>,
    /// Each test function's byte range, and whether it is `#[should_panic]`.
    pub test_functions: Vec<(usize, usize, bool)>,
}

pub fn extract(root: Node<'_>, source: &[u8]) -> RustFacts {
    let mut facts = RustFacts::default();
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "attribute_item" | "inner_attribute_item" => {
                collect_attribute_paths(current, source, &mut facts.attribute_paths);
                // Nothing else of interest lives inside an attribute.
                continue;
            }
            "impl_item" if current.child_by_field_name("trait").is_some() => {
                let type_name = current
                    .child_by_field_name("type")
                    .and_then(|node| implementing_type_name(node, source));
                if let Some(body) = current.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for item in body.named_children(&mut cursor) {
                        if item.kind() == "function_item" {
                            facts.trait_impl_methods.push((
                                type_name.clone(),
                                item.start_byte(),
                                item.end_byte(),
                            ));
                        }
                    }
                }
            }
            "function_item" => {
                if let Some(should_panic) = test_attributes(current, source) {
                    facts.test_functions.push((
                        current.start_byte(),
                        current.end_byte(),
                        should_panic,
                    ));
                }
            }
            _ => {}
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    facts
}

fn collect_attribute_paths(
    attribute: Node<'_>,
    source: &[u8],
    found: &mut Vec<(String, usize, usize)>,
) {
    // An outer attribute decorates the next item that is not itself an
    // attribute or a comment; an inner attribute decorates its parent.
    let target = if attribute.kind() == "inner_attribute_item" {
        attribute.parent()
    } else {
        let mut next = attribute.next_named_sibling();
        while let Some(sibling) = next {
            if sibling.kind().contains("attribute") || sibling.kind().contains("comment") {
                next = sibling.next_named_sibling();
            } else {
                break;
            }
        }
        next
    }
    .unwrap_or(attribute);

    let mut stack = vec![attribute];
    while let Some(node) = stack.pop() {
        if node.kind() == "string_literal" {
            let key = node
                .prev_sibling()
                .filter(|equals| equals.kind() == "=")
                .and_then(|equals| equals.prev_sibling())
                .map(|key| text(key, source))
                .unwrap_or_default();
            if !NON_CODE_KEYS.contains(&key.trim()) {
                let literal = text(node, source);
                let path = literal.trim().trim_matches('"');
                if is_rust_path(path) {
                    found.push((path.to_owned(), target.start_byte(), target.end_byte()));
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// `name`, `module::name`, `Type::method` — a plain Rust path, nothing else.
pub fn is_rust_path(text: &str) -> bool {
    !text.is_empty()
        && text.split("::").all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// `Foo`, `Foo<T>`, `crate::m::Foo<T>` -> `Foo`. References, tuples and bare
/// generic parameters have no single named type here and give `None`.
fn implementing_type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => Some(text(node, source).trim().to_owned()),
        "generic_type" => node
            .child_by_field_name("type")
            .and_then(|inner| implementing_type_name(inner, source)),
        "scoped_type_identifier" => node
            .child_by_field_name("name")
            .and_then(|inner| implementing_type_name(inner, source)),
        _ => None,
    }
}

/// `Some(should_panic)` when the function carries a test attribute.
fn test_attributes(function: Node<'_>, source: &[u8]) -> Option<bool> {
    let mut is_test = false;
    let mut should_panic = false;
    let mut previous = function.prev_named_sibling();
    while let Some(sibling) = previous {
        if sibling.kind() == "attribute_item" {
            match attribute_name(sibling, source).as_deref() {
                Some("test" | "bench" | "rstest" | "test_case") => is_test = true,
                Some("should_panic") => should_panic = true,
                _ => {}
            }
        } else if !sibling.kind().contains("comment") {
            break;
        }
        previous = sibling.prev_named_sibling();
    }
    is_test.then_some(should_panic)
}

/// The last path segment of an attribute: `tokio::test` -> `test`.
fn attribute_name(attribute_item: Node<'_>, source: &[u8]) -> Option<String> {
    let path = attribute_item.named_child(0)?.named_child(0)?;
    let full = text(path, source);
    Some(full.rsplit("::").next().unwrap_or("").trim().to_owned())
}

fn text(node: Node<'_>, source: &[u8]) -> String {
    source
        .get(node.byte_range())
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("rust grammar");
        parser.parse(source, None).expect("parse")
    }

    #[test]
    fn attribute_strings_name_functions() {
        let source = br#"
#[derive(Deserialize)]
struct Config {
    #[serde(default = "default_port", skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(with = "hex_u64")]
    #[cfg(feature = "std")]
    id: u64,
    #[serde(rename = "not a path")]
    other: u8,
}
"#;
        let tree = parse(source);
        let facts = extract(tree.root_node(), source);
        let paths: Vec<&str> = facts
            .attribute_paths
            .iter()
            .map(|(path, _, _)| path.as_str())
            .collect();
        assert!(paths.contains(&"default_port"), "{paths:?}");
        assert!(paths.contains(&"Option::is_none"), "{paths:?}");
        assert!(paths.contains(&"hex_u64"), "{paths:?}");
        assert!(
            !paths.contains(&"std"),
            "a feature name is not code: {paths:?}"
        );
        assert!(!paths.iter().any(|path| path.contains(' ')), "{paths:?}");
    }

    #[test]
    fn methods_of_trait_impls_are_found_with_their_type() {
        let source = br#"
impl<T> SubAssign for Size<T> { fn sub_assign(&mut self, other: Self) {} }
impl Size<u8> { fn inherent(&self) {} }
impl<T: Foo> Bar for T { fn blanket(&self) {} }
"#;
        let tree = parse(source);
        let facts = extract(tree.root_node(), source);
        let names: Vec<(Option<&str>, &str)> = facts
            .trait_impl_methods
            .iter()
            .map(|(ty, start, end)| {
                let body = std::str::from_utf8(&source[*start..*end]).unwrap();
                let name = body
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .split('(')
                    .next()
                    .unwrap();
                (ty.as_deref(), name)
            })
            .collect();
        assert!(names.contains(&(Some("Size"), "sub_assign")), "{names:?}");
        assert!(names.contains(&(Some("T"), "blanket")), "{names:?}");
        assert!(
            !names.iter().any(|(_, name)| *name == "inherent"),
            "{names:?}"
        );
    }

    #[test]
    fn test_attributes_decide_what_is_a_test() {
        let source = br#"
#[test]
fn plain() {}
#[tokio::test]
async fn asynchronous() {}
#[test]
#[should_panic(expected = "boom")]
fn panics() {}
fn test_helper() {}
"#;
        let tree = parse(source);
        let facts = extract(tree.root_node(), source);
        assert_eq!(facts.test_functions.len(), 3);
        assert_eq!(
            facts
                .test_functions
                .iter()
                .filter(|(_, _, should_panic)| *should_panic)
                .count(),
            1
        );
    }
}
