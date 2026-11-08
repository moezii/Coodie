//! JSX rendering and React Router entrypoints.
//!
//! A React application is invisible to a plain call graph. Components are not
//! called — they are *rendered* (`<BookingPage />`), and routes are declared as
//! markup (`<Route path="/bookings" element={<BookingPage />} />`) rather than
//! registered through a function call.
//!
//! Without this, an entire frontend reports as unreachable. On a real rental
//! application that was 457 of 590 unresolved symbols — the audit was blind to
//! half the product.

use tree_sitter::Node;

use super::graph::CallSite;
use super::{EntryPoint, EntryPointKind, Exposure};
use crate::index::{LineRange, SourceLanguage};
use std::path::Path;

/// Element names that declare a route rather than render a component.
const ROUTE_ELEMENTS: &[&str] = &["Route", "PrivateRoute", "ProtectedRoute", "AuthRoute"];

pub fn supports_jsx(language: &SourceLanguage) -> bool {
    matches!(language.id(), "javascript" | "tsx" | "typescript")
}

pub struct JsxFacts {
    /// `<Foo />` treated as a reference from the enclosing symbol to `Foo`.
    pub renders: Vec<(String, usize, usize)>,
    pub entrypoints: Vec<EntryPoint>,
}

/// Walk a parsed tree for JSX usage.
pub fn extract(language: &SourceLanguage, path: &Path, source: &[u8], root: Node<'_>) -> JsxFacts {
    let mut renders = Vec::new();
    let mut entrypoints = Vec::new();

    if !supports_jsx(language) {
        return JsxFacts {
            renders,
            entrypoints,
        };
    }

    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let kind = current.kind();
        if kind == "jsx_self_closing_element" || kind == "jsx_opening_element" {
            if let Some(name_node) = current.child_by_field_name("name") {
                let name = node_text(name_node, source);
                // Lowercase names are HTML tags (`div`, `span`), not components.
                if starts_uppercase(&name) {
                    if ROUTE_ELEMENTS.contains(&name.as_str()) {
                        if let Some(entrypoint) = route_entrypoint(current, path, source) {
                            entrypoints.push(entrypoint);
                        }
                    } else {
                        renders.push((name, current.start_byte(), current.end_byte()));
                    }
                }
            }
        }

        // `onClick={closeModal}` and `{items.map(renderRow)}` reference real
        // functions that run when a user interacts with the page. They are not
        // calls in the syntax, so without this every event handler in a React
        // codebase reports as dead code.
        if kind == "jsx_expression" {
            collect_expression_identifiers(current, source, &mut renders);
        }

        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    renders.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    renders.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
    entrypoints.sort_by_key(|entrypoint| entrypoint.line_range.start);
    entrypoints.dedup_by(|left, right| left.name == right.name && left.route == right.route);

    JsxFacts {
        renders,
        entrypoints,
    }
}

/// Collect identifiers used inside a JSX `{...}` expression.
fn collect_expression_identifiers(
    node: Node<'_>,
    source: &[u8],
    found: &mut Vec<(String, usize, usize)>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let kind = current.kind();
        // Nested JSX is handled by the outer walk; do not double-count it.
        if current.id() != node.id()
            && (kind == "jsx_element"
                || kind == "jsx_self_closing_element"
                || kind == "jsx_opening_element")
        {
            continue;
        }
        if kind == "identifier" || kind == "property_identifier" {
            let name = node_text(current, source);
            if !name.is_empty() {
                found.push((name, current.start_byte(), current.end_byte()));
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

/// Build an entrypoint from `<Route path="/x" element={<Page />} />`.
fn route_entrypoint(node: Node<'_>, path: &Path, source: &[u8]) -> Option<EntryPoint> {
    let route = jsx_attribute_text(node, "path", source)?;
    let handler = jsx_attribute_component(node, "element", source)
        .or_else(|| jsx_attribute_component(node, "component", source));

    Some(EntryPoint {
        kind: EntryPointKind::Application,
        name: format!("PAGE {route}"),
        exposure: Exposure::External,
        http_method: None,
        route: Some(route),
        handler_name: handler,
        file: path.to_path_buf(),
        line_range: LineRange {
            start: node.start_position().row + 1,
            end: node.end_position().row + 1,
        },
        framework: "react-router".to_owned(),
    })
}

/// Read a string-literal JSX attribute: `path="/bookings"` -> `/bookings`.
fn jsx_attribute_text(node: Node<'_>, attribute: &str, source: &[u8]) -> Option<String> {
    let value = jsx_attribute_value(node, attribute, source)?;
    let mut stack = vec![value];
    while let Some(current) = stack.pop() {
        if current.kind().contains("string") {
            let text = node_text(current, source);
            return Some(strip_quotes(&text));
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// Read the component out of `element={<BookingPage />}`.
fn jsx_attribute_component(node: Node<'_>, attribute: &str, source: &[u8]) -> Option<String> {
    let value = jsx_attribute_value(node, attribute, source)?;
    let mut stack = vec![value];
    while let Some(current) = stack.pop() {
        let kind = current.kind();
        if kind == "jsx_self_closing_element" || kind == "jsx_opening_element" {
            if let Some(name_node) = current.child_by_field_name("name") {
                let name = node_text(name_node, source);
                if starts_uppercase(&name) {
                    return Some(name);
                }
            }
        }
        if kind == "identifier" {
            let name = node_text(current, source);
            if starts_uppercase(&name) {
                return Some(name);
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

fn jsx_attribute_value<'tree>(
    node: Node<'tree>,
    attribute: &str,
    source: &[u8],
) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "jsx_attribute" {
            continue;
        }
        let name = child.named_child(0)?;
        if node_text(name, source) != attribute {
            continue;
        }
        return child.named_child(1);
    }
    None
}

/// Turn rendered component names into call sites from the enclosing symbol.
pub fn renders_as_calls(
    renders: &[(String, usize, usize)],
    relative: &str,
    enclosing: impl Fn(usize, usize) -> Option<(String, String)>,
) -> Vec<CallSite> {
    renders
        .iter()
        .map(|(name, start, end)| {
            let owner = enclosing(*start, *end);
            CallSite {
                caller: owner.as_ref().map(|(name, _)| name.clone()),
                callee: name.clone(),
                qualifier: None,
                caller_scope: owner.map(|(_, scope)| scope),
                is_reference: false,
                file: relative.to_owned(),
            }
        })
        .collect()
}

fn starts_uppercase(value: &str) -> bool {
    value
        .chars()
        .next()
        .map(|character| character.is_ascii_uppercase())
        .unwrap_or(false)
}

fn node_text(node: Node<'_>, source: &[u8]) -> String {
    source
        .get(node.byte_range())
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

fn strip_quotes(value: &str) -> String {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'' || first == b'`') && first == last {
            return trimmed[1..trimmed.len() - 1].to_owned();
        }
    }
    trimmed.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tree_sitter::Parser;

    fn parse_jsx(source: &[u8]) -> (SourceLanguage, tree_sitter::Tree) {
        let language = SourceLanguage::new("javascript").expect("javascript");
        let mut parser = Parser::new();
        parser
            .set_language(&language.tree_sitter_language().expect("grammar"))
            .expect("set language");
        let tree = parser.parse(source, None).expect("parse");
        (language, tree)
    }

    #[test]
    fn finds_react_router_routes_as_entrypoints() {
        let source = br#"
export default function App() {
  return (
    <Routes>
      <Route path="/bookings" element={<BookingPage />} />
      <Route path="/cars/:id" element={<CarDetail />} />
    </Routes>
  );
}
"#;
        let (language, tree) = parse_jsx(source);
        let facts = extract(&language, &PathBuf::from("App.jsx"), source, tree.root_node());

        let routes: Vec<&str> = facts
            .entrypoints
            .iter()
            .filter_map(|e| e.route.as_deref())
            .collect();
        assert!(routes.contains(&"/bookings"), "got {routes:?}");
        assert!(routes.contains(&"/cars/:id"), "got {routes:?}");

        let booking = facts
            .entrypoints
            .iter()
            .find(|e| e.route.as_deref() == Some("/bookings"))
            .expect("route found");
        assert_eq!(booking.handler_name.as_deref(), Some("BookingPage"));
    }

    #[test]
    fn rendered_components_become_references() {
        let source = br#"
function Dashboard() {
  return <div><StatsPanel /><RecentBookings /></div>;
}
"#;
        let (language, tree) = parse_jsx(source);
        let facts = extract(&language, &PathBuf::from("Dashboard.jsx"), source, tree.root_node());
        let names: Vec<&str> = facts
            .renders
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();

        assert!(names.contains(&"StatsPanel"), "got {names:?}");
        assert!(names.contains(&"RecentBookings"), "got {names:?}");
        // Lowercase `div` is an HTML tag, not a component.
        assert!(!names.contains(&"div"), "html tags must be ignored: {names:?}");
    }

    #[test]
    fn event_handlers_passed_in_jsx_are_references_not_dead_code() {
        let source = br#"
function Modal() {
  const closeModal = () => setOpen(false);
  return <button onClick={closeModal}>Close</button>;
}
"#;
        let (language, tree) = parse_jsx(source);
        let facts = extract(&language, &PathBuf::from("Modal.jsx"), source, tree.root_node());
        let names: Vec<&str> = facts
            .renders
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();

        assert!(
            names.contains(&"closeModal"),
            "an onClick handler is invoked at runtime: {names:?}"
        );
    }

    #[test]
    fn route_elements_are_not_also_counted_as_rendered_components() {
        let source = br#"<Route path="/x" element={<Page />} />"#;
        let (language, tree) = parse_jsx(source);
        let facts = extract(&language, &PathBuf::from("r.jsx"), source, tree.root_node());

        assert_eq!(facts.entrypoints.len(), 1);
        let names: Vec<&str> = facts
            .renders
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert!(!names.contains(&"Route"), "Route is a declaration: {names:?}");
        // The page inside element={} is still a rendered component.
        assert!(names.contains(&"Page"), "got {names:?}");
    }
}
