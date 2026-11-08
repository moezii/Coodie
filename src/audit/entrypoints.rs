//! Entrypoint extraction.
//!
//! An entrypoint is a place the outside world can reach the software: an HTTP
//! route, a CLI command, a queue consumer, a scheduled job, or the process
//! startup path. Everything downstream — reachability, coverage, the verdict —
//! depends on knowing where the outside world gets in.
//!
//! Detection is convention-driven (see [`super::conventions`]) and always
//! evidence-backed: a match must carry a real file and line range.

use std::path::Path;
use std::sync::OnceLock;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

use super::conventions::{
    call_convention, decorator_convention, looks_like_route, receiver_matches, receivers_for,
};
use super::{EntryPoint, EntryPointKind, Exposure};
use crate::index::{LineRange, SourceLanguage};

const JAVASCRIPT_REGISTRATIONS: &str = include_str!("queries/javascript_registrations.scm");
const PYTHON_REGISTRATIONS: &str = include_str!("queries/python_registrations.scm");
const GO_REGISTRATIONS: &str = include_str!("queries/go_registrations.scm");
const JAVA_REGISTRATIONS: &str = include_str!("queries/java_registrations.scm");
const TYPESCRIPT_REGISTRATIONS: &str = include_str!("queries/typescript_registrations.scm");
const RUST_REGISTRATIONS: &str = include_str!("queries/rust_registrations.scm");
const CSHARP_REGISTRATIONS: &str = include_str!("queries/csharp_registrations.scm");
const RUBY_REGISTRATIONS: &str = include_str!("queries/ruby_registrations.scm");
const PHP_REGISTRATIONS: &str = include_str!("queries/php_registrations.scm");

/// Languages with entrypoint support. Everything else still receives baseline
/// symbol analysis — it just cannot contribute routes or jobs.
pub fn has_entrypoint_support(language: &SourceLanguage) -> bool {
    registration_source(language).is_some()
}

fn registration_source(language: &SourceLanguage) -> Option<&'static str> {
    match language.id() {
        "javascript" => Some(JAVASCRIPT_REGISTRATIONS),
        "typescript" | "tsx" => Some(TYPESCRIPT_REGISTRATIONS),
        "python" => Some(PYTHON_REGISTRATIONS),
        "go" => Some(GO_REGISTRATIONS),
        "java" => Some(JAVA_REGISTRATIONS),
        "rust" => Some(RUST_REGISTRATIONS),
        "csharp" => Some(CSHARP_REGISTRATIONS),
        "ruby" => Some(RUBY_REGISTRATIONS),
        "php" => Some(PHP_REGISTRATIONS),
        _ => None,
    }
}

fn registration_query(language: &SourceLanguage) -> Option<&'static Query> {
    macro_rules! cached {
        ($cell:ident, $source:expr) => {{
            static $cell: OnceLock<Option<Query>> = OnceLock::new();
            $cell
                .get_or_init(|| {
                    let ts_language = language.tree_sitter_language()?;
                    Query::new(&ts_language, $source).ok()
                })
                .as_ref()
        }};
    }

    match language.id() {
        "javascript" => cached!(JAVASCRIPT_QUERY, JAVASCRIPT_REGISTRATIONS),
        "typescript" => cached!(TYPESCRIPT_QUERY, TYPESCRIPT_REGISTRATIONS),
        "tsx" => cached!(TSX_QUERY, TYPESCRIPT_REGISTRATIONS),
        "python" => cached!(PYTHON_QUERY, PYTHON_REGISTRATIONS),
        "go" => cached!(GO_QUERY, GO_REGISTRATIONS),
        "java" => cached!(JAVA_QUERY, JAVA_REGISTRATIONS),
        "rust" => cached!(RUST_QUERY, RUST_REGISTRATIONS),
        "csharp" => cached!(CSHARP_QUERY, CSHARP_REGISTRATIONS),
        "ruby" => cached!(RUBY_QUERY, RUBY_REGISTRATIONS),
        "php" => cached!(PHP_QUERY, PHP_REGISTRATIONS),
        _ => None,
    }
}

/// Extract entrypoints from an already-parsed tree.
///
/// The audit parses each file once and hands the same tree to symbol, call, and
/// entrypoint extraction, so this is the entry point the pipeline uses.
pub fn extract_from_tree(
    language: &SourceLanguage,
    path: &Path,
    source: &[u8],
    root: Node<'_>,
) -> Vec<EntryPoint> {
    // A language without a registration file still has a way in: every binary
    // has a main. Returning early here left C, C++, Scala and OCaml with no
    // entrypoints at all, so nothing in them could be proven reachable.
    let Some(query) = registration_query(language) else {
        return startup_entrypoints(language, path, source, root);
    };

    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut entrypoints = Vec::new();

    let mut matches = cursor.matches(query, root, source);
    while let Some(matched) = matches.next() {
        let mut site = None;
        let mut receiver = None;
        let mut method = None;
        let mut args = None;
        let mut handler = None;
        let mut decorators = Vec::new();

        for capture in matched.captures {
            let name = capture_names
                .get(capture.index as usize)
                .copied()
                .unwrap_or("");
            match name {
                "reg.site" => site = Some(capture.node),
                "reg.receiver" => receiver = Some(capture.node),
                "reg.method" => method = Some(capture.node),
                "reg.args" => args = Some(capture.node),
                "reg.handler" => handler = Some(capture.node),
                "reg.decorator" => decorators.push(capture.node),
                _ => {}
            }
        }

        let Some(site) = site else { continue };

        if !decorators.is_empty() {
            for decorator in decorators {
                if let Some(entrypoint) = entrypoint_from_decorator(
                    decorator, handler, site, path, source,
                ) {
                    entrypoints.push(entrypoint);
                }
            }
            continue;
        }

        if let Some(method) = method {
            if let Some(entrypoint) = entrypoint_from_call(
                receiver, method, args, handler, site, path, source,
            ) {
                entrypoints.push(entrypoint);
            }
        }
    }

    entrypoints.extend(startup_entrypoints(language, path, source, root));

    // A decorated Python route matches twice: once as the decorated definition
    // (which knows the handler) and once as the bare call inside the decorator
    // (which does not). Order handler-bearing records first so the dedup below
    // keeps the richer one.
    entrypoints.sort_by(|left, right| {
        left.line_range
            .start
            .cmp(&right.line_range.start)
            .then(left.name.cmp(&right.name))
            .then(
                left.handler_name
                    .is_none()
                    .cmp(&right.handler_name.is_none()),
            )
    });
    entrypoints.dedup_by(|left, right| {
        left.line_range.start == right.line_range.start && left.name == right.name
    });
    entrypoints
}

/// Parse `source` and extract its entrypoints. Convenience wrapper for callers
/// that do not already hold a tree.
pub fn extract_entrypoints(
    language: &SourceLanguage,
    path: &Path,
    source: &[u8],
) -> Vec<EntryPoint> {
    let Some(ts_language) = language.tree_sitter_language() else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts_language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    extract_from_tree(language, path, source, tree.root_node())
}

fn entrypoint_from_call(
    receiver: Option<Node<'_>>,
    method: Node<'_>,
    args: Option<Node<'_>>,
    handler: Option<Node<'_>>,
    site: Node<'_>,
    path: &Path,
    source: &[u8],
) -> Option<EntryPoint> {
    // A call inside a decorator is not an independent registration — the
    // decorated definition already covers it, and only that form knows the real
    // handler. Without this, `@router.post("/x", response_model=Foo)` produces a
    // second entrypoint whose "handler" is scavenged from the decorator's own
    // arguments, mislabelling every route in a FastAPI application.
    if handler.is_none() && inside_decorator(site) {
        return None;
    }

    let method_name = node_text(method, source);

    // Annotation shape: no receiver, but a handler the annotation sits above.
    // `@GetMapping("/x")` in Java and `[HttpGet("/x")]` in C# both arrive here,
    // and both are decorator conventions rather than call conventions.
    if receiver.is_none() && handler.is_some() {
        if let Some(convention) = decorator_convention(&method_name) {
            let arguments = args
                .map(|node| argument_values(node, source))
                .unwrap_or_default();
            let route = arguments.iter().find_map(|argument| match argument {
                Argument::Text(value) => Some(value.clone()),
                _ => None,
            });
            let handler_name = handler.map(|node| node_text(node, source));
            let name = entrypoint_name(
                convention.kind,
                convention.http_method,
                route.as_deref(),
                handler_name.as_deref(),
                &method_name,
            );
            return Some(EntryPoint {
                kind: convention.kind,
                name,
                exposure: Exposure::External,
                http_method: convention.http_method.map(str::to_owned),
                route,
                handler_name,
                file: path.to_path_buf(),
                line_range: line_range(site),
                framework: convention.framework.to_owned(),
            });
        }
    }

    let convention = call_convention(&method_name)?;

    // Receiver allowlist: this is what stops `cache.get(key)` and
    // `results.delete(id)` from being read as HTTP routes.
    let allowed = receivers_for(convention.kind);
    let receiver_text = receiver.map(|node| node_text(node, source));
    // The route shape is the stronger signal. `myBackend.get("/users")` is a
    // route whatever the object is called, and a receiver allowlist alone means
    // a silently missed endpoint every time someone names their app object
    // something unexpected — the worst failure this tool has, because
    // everything downstream depends on knowing the ways in.
    //
    // So for route-bearing conventions the allowlist is a tiebreaker, and the
    // `/...` first argument checked below is what actually decides. For
    // conventions with no route to check — `listen`, `on`, `command` — the
    // allowlist is still required, since nothing else distinguishes them from
    // any other method of the same name.
    if !allowed.is_empty() && !convention.requires_route {
        match &receiver_text {
            Some(text) if receiver_matches(text, allowed) => {}
            _ => return None,
        }
    }

    let arguments = args.map(|node| argument_values(node, source)).unwrap_or_default();
    let route = arguments.iter().find_map(|argument| match argument {
        Argument::Text(value) => Some(value.clone()),
        Argument::Name(_) | Argument::Inline => None,
    });

    if convention.requires_route {
        let route_value = route.as_deref()?;
        if !looks_like_route(route_value) {
            return None;
        }
        // An unfamiliar receiver is accepted only when the call looks like a
        // registration rather than a client request. Both shapes are
        // `something.get("/path", ...)`, so the receiver name cannot decide it
        // and neither can the route.
        //
        // What separates the two is what happens at the two ends of the call.
        // A registration hands over a *function* to be called back later and
        // throws the return value away. A client request passes options rather
        // than a callback, and always uses what comes back.
        //
        // A decorator is a registration too: `@app.get("/x")` names no handler
        // in its arguments because the function underneath it is the handler.
        let passes_a_function =
            handler.is_some() || arguments.iter().any(|a| matches!(a, Argument::Inline));
        let is_registration_shape = passes_a_function || inside_decorator(site);

        // Universal, and independent of the receiver name: a call whose result
        // is consumed - awaited, returned, assigned, or chained into `.then` -
        // and that hands over no function is a request being made, not a route
        // being served. `api` and `client` sit in the allowlist precisely
        // because they are common on both sides.
        if !is_registration_shape && !result_is_discarded(site) {
            return None;
        }

        let recognised_receiver = receiver_text
            .as_deref()
            .map(|text| receiver_matches(text, allowed))
            .unwrap_or(true);
        let has_handler = is_registration_shape
            || arguments.iter().any(|a| matches!(a, Argument::Name(_)));
        if !recognised_receiver && !has_handler {
            return None;
        }
    }

    let handler_name = handler
        .map(|node| node_text(node, source))
        .or_else(|| {
            arguments.iter().find_map(|argument| match argument {
                Argument::Name(value) => Some(trailing_segment(value)),
                _ => None,
            })
        });

    let kind = convention.kind;
    let exposure = if kind == EntryPointKind::Startup {
        Exposure::Startup
    } else {
        Exposure::External
    };
    let name = entrypoint_name(
        kind,
        convention.http_method,
        route.as_deref(),
        handler_name.as_deref(),
        &method_name,
    );

    Some(EntryPoint {
        kind,
        name,
        exposure,
        http_method: convention.http_method.map(str::to_owned),
        route,
        handler_name,
        file: path.to_path_buf(),
        line_range: line_range(site),
        framework: convention.framework.to_owned(),
    })
}

fn entrypoint_from_decorator(
    decorator: Node<'_>,
    handler: Option<Node<'_>>,
    site: Node<'_>,
    path: &Path,
    source: &[u8],
) -> Option<EntryPoint> {
    let text = node_text(decorator, source);
    let parsed = parse_decorator(&text)?;
    let convention = decorator_convention(&parsed.name)?;

    let handler_name = handler.map(|node| node_text(node, source));
    let kind = convention.kind;
    let name = entrypoint_name(
        kind,
        convention.http_method,
        parsed.route.as_deref(),
        handler_name.as_deref(),
        &parsed.name,
    );

    Some(EntryPoint {
        kind,
        name,
        exposure: Exposure::External,
        http_method: convention.http_method.map(str::to_owned),
        route: parsed.route,
        handler_name,
        file: path.to_path_buf(),
        line_range: line_range(site),
        framework: convention.framework.to_owned(),
    })
}

/// Byte ranges of Python's module-level `if __name__ == "__main__":` blocks.
///
/// The block is the script's entrypoint, but it sits outside every function, so
/// the calls inside it have no enclosing symbol to belong to. The audit
/// attributes them to the block's entrypoint instead.
pub fn python_main_blocks(
    language: &SourceLanguage,
    source: &[u8],
    root: Node<'_>,
) -> Vec<(usize, usize)> {
    if language.id() != "python" {
        return Vec::new();
    }
    let mut cursor = root.walk();
    root.children(&mut cursor)
        .filter(|child| child.kind() == "if_statement" && is_python_main_guard(*child, source))
        .map(|child| (child.start_byte(), child.end_byte()))
        .collect()
}

/// Is this `if` statement the `__name__ == "__main__"` guard? Only its
/// condition is read: a block that merely mentions `__main__` somewhere in
/// its body is not the guard.
fn is_python_main_guard(node: Node<'_>, source: &[u8]) -> bool {
    let condition = node
        .child_by_field_name("condition")
        .map(|condition| node_text(condition, source))
        .unwrap_or_default();
    condition.contains("__name__") && condition.contains("__main__")
}

/// Process startup paths that are structural rather than convention-based:
/// a top-level `main` function, or Python's `if __name__ == "__main__":`.
fn startup_entrypoints(
    language: &SourceLanguage,
    path: &Path,
    source: &[u8],
    root: Node<'_>,
) -> Vec<EntryPoint> {
    let mut found = Vec::new();

    if language.id() == "python" {
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() != "if_statement" {
                continue;
            }
            if is_python_main_guard(child, source) {
                found.push(EntryPoint {
                    kind: EntryPointKind::Cli,
                    name: "__main__".to_owned(),
                    exposure: Exposure::External,
                    http_method: None,
                    route: None,
                    handler_name: None,
                    file: path.to_path_buf(),
                    line_range: line_range(child),
                    framework: "python".to_owned(),
                });
            }
        }
    }

    // Every language whose binaries start at a top-level `main`. Kotlin and
    // Swift are here because an Android or iOS module otherwise has no way in
    // at all, which makes everything in it read as unreachable.
    let main_kinds = match language.id() {
        "go" | "rust" | "c" | "cpp" | "kotlin" | "swift" | "scala" | "java" | "csharp" => {
            Some(("function_declaration", "function_item"))
        }
        _ => None,
    };
    if main_kinds.is_some() {
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            let is_function = matches!(
                child.kind(),
                "function_declaration" | "function_item" | "function_definition"
            );
            if !is_function {
                continue;
            }
            // Not every grammar exposes the name as a field. C and C++ nest it
            // in a declarator; Kotlin and Swift make it a bare
            // `simple_identifier` child. Without both fallbacks the language
            // has no way in at all and everything in it reads as unreachable.
            let Some(name_node) = child
                .child_by_field_name("name")
                .or_else(|| declarator_identifier(child))
                .or_else(|| first_identifier_child(child))
            else {
                continue;
            };
            if node_text(name_node, source) != "main" {
                continue;
            }
            // A binary's `main` is genuinely invoked from outside the process,
            // so it is an external entrypoint rather than startup wiring. For a
            // CLI it is *the* entrypoint; for a server it sits alongside the
            // routes and makes the startup path reachable.
            found.push(EntryPoint {
                kind: EntryPointKind::Cli,
                name: "main".to_owned(),
                exposure: Exposure::External,
                http_method: None,
                route: None,
                handler_name: Some("main".to_owned()),
                file: path.to_path_buf(),
                line_range: line_range(child),
                framework: language.id().to_owned(),
            });
        }
    }

    found
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedDecorator {
    name: String,
    route: Option<String>,
}

/// Parse `@app.get("/users")` into name `get` and route `/users`.
/// Java's `@GetMapping("/x")` and bare `@Scheduled` both fall out of the same rules.
fn parse_decorator(text: &str) -> Option<ParsedDecorator> {
    let trimmed = text.trim().trim_start_matches('@').trim();
    if trimmed.is_empty() {
        return None;
    }
    let (path, argument_text) = match trimmed.find('(') {
        Some(index) => (
            &trimmed[..index],
            trimmed[index..].trim_start_matches('(').trim_end_matches(')'),
        ),
        None => (trimmed, ""),
    };
    let name = trailing_segment(path.trim());
    if name.is_empty() {
        return None;
    }
    Some(ParsedDecorator {
        name,
        route: first_string_literal(argument_text),
    })
}

/// Pull the first quoted string out of raw argument text.
fn first_string_literal(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let quote = bytes[index];
        if quote == b'"' || quote == b'\'' {
            let start = index + 1;
            let mut cursor = start;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    cursor += 2;
                    continue;
                }
                if bytes[cursor] == quote {
                    return Some(text[start..cursor].to_owned());
                }
                cursor += 1;
            }
            return None;
        }
        index += 1;
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Argument {
    /// A string literal, quotes already stripped.
    Text(String),
    /// A named reference such as `handleLogin` or `controllers.login`.
    Name(String),
    /// An inline function/lambda with no name to cite.
    Inline,
}

fn argument_values(node: Node<'_>, source: &[u8]) -> Vec<Argument> {
    let mut values = Vec::new();
    collect_arguments(node, source, 0, &mut values);
    values
}

/// Walk an argument list, descending through wrapper nodes.
///
/// Grammars disagree about how deeply arguments nest: C# wraps each one in an
/// `attribute_argument`, Java in an `element_value_pair`, Rust attribute macros
/// in a `token_tree`. Rather than special-casing each, unrecognised nodes are
/// descended into up to a bounded depth.
fn collect_arguments(node: Node<'_>, source: &[u8], depth: usize, values: &mut Vec<Argument>) {
    const MAX_DEPTH: usize = 3;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        // A keyword argument's *name* is not an argument value. Without this,
        // `@router.post("/x", response_model=Foo)` yields `response_model` as
        // the handler, silently mislabelling every route in a FastAPI app.
        if kind == "keyword_argument"
            || kind == "named_argument"
            || kind == "element_value_pair"
            || kind == "argument"
        {
            if let Some(value) = child.child_by_field_name("value") {
                if depth < MAX_DEPTH {
                    collect_arguments_from_node(value, source, depth + 1, values);
                }
            } else if depth < MAX_DEPTH {
                collect_arguments(child, source, depth + 1, values);
            }
            continue;
        }
        if kind.contains("string") {
            values.push(Argument::Text(strip_quotes(&node_text(child, source))));
        } else if kind.contains("function") || kind.contains("lambda") || kind.contains("closure") {
            values.push(Argument::Inline);
        } else if kind.contains("identifier")
            || kind == "member_expression"
            || kind == "attribute"
            || kind == "selector_expression"
            || kind == "field_access"
        {
            values.push(Argument::Name(node_text(child, source)));
        } else if depth < MAX_DEPTH {
            collect_arguments(child, source, depth + 1, values);
        }
    }
}

/// Classify a single node as an argument value, descending if it is a wrapper.
fn collect_arguments_from_node(
    node: Node<'_>,
    source: &[u8],
    depth: usize,
    values: &mut Vec<Argument>,
) {
    let kind = node.kind();
    if kind.contains("string") {
        values.push(Argument::Text(strip_quotes(&node_text(node, source))));
    } else if kind.contains("function") || kind.contains("lambda") || kind.contains("closure") {
        values.push(Argument::Inline);
    } else if kind.contains("identifier")
        || kind == "member_expression"
        || kind == "attribute"
        || kind == "selector_expression"
        || kind == "field_access"
    {
        values.push(Argument::Name(node_text(node, source)));
    } else {
        collect_arguments(node, source, depth, values);
    }
}

fn entrypoint_name(
    kind: EntryPointKind,
    http_method: Option<&str>,
    route: Option<&str>,
    handler: Option<&str>,
    fallback: &str,
) -> String {
    match kind {
        EntryPointKind::Http | EntryPointKind::WebSocket => {
            let verb = http_method.unwrap_or("ANY");
            match route {
                Some(route) => format!("{verb} {route}"),
                None => format!("{verb} {}", handler.unwrap_or(fallback)),
            }
        }
        _ => route
            .map(str::to_owned)
            .or_else(|| handler.map(str::to_owned))
            .unwrap_or_else(|| fallback.to_owned()),
    }
}

/// Is this node inside a decorator or attribute annotation?
fn inside_decorator(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(value) = current {
        let kind = value.kind();
        if kind == "decorator" || kind == "attribute_item" || kind == "attribute_list" {
            return true;
        }
        current = value.parent();
    }
    false
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

/// `controllers.login` -> `login`; `app` -> `app`.
fn trailing_segment(value: &str) -> String {
    value
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(value)
        .trim()
        .to_owned()
}

fn line_range(node: Node<'_>) -> LineRange {
    LineRange {
        start: node.start_position().row + 1,
        end: node.end_position().row + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn language(id: &str) -> SourceLanguage {
        SourceLanguage::new(id).expect("known language")
    }

    /// A malformed `.scm` makes `Query::new` fail, and `extract_from_tree` then
    /// silently returns nothing — an entire language would go dark with no
    /// error. This test is the guard against that.
    #[test]
    fn every_registration_query_compiles() {
        for id in [
            "javascript",
            "typescript",
            "tsx",
            "python",
            "go",
            "java",
            "rust",
            "csharp",
            "ruby",
            "php",
        ] {
            let source_language = language(id);
            let ts_language = source_language
                .tree_sitter_language()
                .unwrap_or_else(|| panic!("{id} has no tree-sitter grammar"));
            let source = registration_source(&source_language)
                .unwrap_or_else(|| panic!("{id} has no registration query"));
            if let Err(error) = tree_sitter::Query::new(&ts_language, source) {
                panic!("registration query for {id} does not compile: {error:?}");
            }
            assert!(
                registration_query(&source_language).is_some(),
                "{id} registration query failed to build"
            );
        }
    }

    #[test]
    fn parses_python_decorator_with_route() {
        let parsed = parse_decorator("@app.get(\"/users/{id}\")").expect("parsed");
        assert_eq!(parsed.name, "get");
        assert_eq!(parsed.route.as_deref(), Some("/users/{id}"));
    }

    #[test]
    fn parses_bare_annotation() {
        let parsed = parse_decorator("@Scheduled").expect("parsed");
        assert_eq!(parsed.name, "Scheduled");
        assert_eq!(parsed.route, None);
    }

    #[test]
    fn finds_express_routes_and_ignores_config_getter() {
        let source = br#"
const app = express();
app.set("view engine", "pug");
app.get("trust proxy");
app.get("/users", listUsers);
app.post("/users", createUser);
app.listen(3000);
"#;
        let found = extract_entrypoints(&language("javascript"), &PathBuf::from("server.js"), source);
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"GET /users"), "got {names:?}");
        assert!(names.contains(&"POST /users"), "got {names:?}");
        // "trust proxy" is not a route, so it must not become an endpoint.
        assert!(
            !names.iter().any(|name| name.contains("trust proxy")),
            "config getter leaked into entrypoints: {names:?}"
        );
        assert!(
            found.iter().any(|e| e.kind == EntryPointKind::Startup),
            "app.listen should be a startup entrypoint"
        );
    }

    #[test]
    fn resolves_handler_names_for_routes() {
        let source = br#"app.get("/health", healthCheck);"#;
        let found = extract_entrypoints(&language("javascript"), &PathBuf::from("a.js"), source);
        let route = found
            .iter()
            .find(|e| e.kind == EntryPointKind::Http)
            .expect("route found");
        assert_eq!(route.handler_name.as_deref(), Some("healthCheck"));
        assert_eq!(route.http_method.as_deref(), Some("GET"));
    }

    #[test]
    fn finds_fastapi_decorated_routes() {
        let source = br#"
@app.get("/items")
def list_items():
    return []

@router.post("/items")
def create_item():
    return {}
"#;
        let found = extract_entrypoints(&language("python"), &PathBuf::from("api.py"), source);
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"GET /items"), "got {names:?}");
        assert!(names.contains(&"POST /items"), "got {names:?}");
        assert_eq!(
            found
                .iter()
                .find(|e| e.name == "GET /items")
                .and_then(|e| e.handler_name.as_deref()),
            Some("list_items")
        );
    }

    #[test]
    fn finds_async_routes_with_multiline_decorators() {
        // The real FastAPI shape: async handlers, decorator keyword arguments,
        // and parameters spread over several lines.
        let source = br#"
@router.post("/validate", response_model=BookingValidation)
async def validate_booking(
    booking_data: BookingCreate,
    company_id: int = Depends(get_current_company_id)
):
    return await booking_controller.validate_booking(booking_data)

@router.get(
    "/{booking_id}",
    response_model=BookingResponse,
)
async def get_booking(booking_id: str):
    return await booking_controller.get_booking(booking_id)
"#;
        let found = extract_entrypoints(&language("python"), &PathBuf::from("routes/bookings.py"), source);

        let validate = found
            .iter()
            .find(|e| e.route.as_deref() == Some("/validate"))
            .unwrap_or_else(|| panic!("POST /validate not found: {found:?}"));
        assert_eq!(
            validate.handler_name.as_deref(),
            Some("validate_booking"),
            "async handler must be linked: {found:?}"
        );

        let get = found
            .iter()
            .find(|e| e.route.as_deref() == Some("/{booking_id}"))
            .unwrap_or_else(|| panic!("GET /{{booking_id}} not found: {found:?}"));
        assert_eq!(
            get.handler_name.as_deref(),
            Some("get_booking"),
            "multi-line decorator handler must be linked: {found:?}"
        );
    }

    #[test]
    fn finds_laravel_style_php_routes() {
        let source = br#"<?php
Route::get('/users', [UserController::class, 'index']);
Route::post('/users', [UserController::class, 'store']);
$app->delete('/users/{id}', 'UserController@destroy');
"#;
        let found = extract_entrypoints(&language("php"), &PathBuf::from("routes/web.php"), source);
        let routes: Vec<_> = found.iter().filter_map(|e| e.route.as_deref()).collect();

        assert!(routes.contains(&"/users"), "got {found:?}");
        assert!(routes.contains(&"/users/{id}"), "got {found:?}");
        assert!(
            found
                .iter()
                .any(|e| e.http_method.as_deref() == Some("DELETE")),
            "the receiver-style call should resolve its method: {found:?}"
        );
    }

    #[test]
    fn finds_nestjs_decorated_routes_in_typescript() {
        let source = br#"
@Controller('users')
export class UsersController {
  @Get('/profile')
  getProfile() {
    return this.service.profile();
  }

  @Post('/profile')
  updateProfile() {
    return this.service.update();
  }
}
"#;
        let found = extract_entrypoints(
            &language("typescript"),
            &PathBuf::from("users.controller.ts"),
            source,
        );
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"GET /profile"), "got {names:?}");
        assert!(names.contains(&"POST /profile"), "got {names:?}");
    }

    #[test]
    fn finds_express_routes_in_tsx_files() {
        // A .tsx file is still TypeScript; entrypoint detection must not fall
        // back to baseline just because JSX is present.
        let source = br#"
const app = express();
app.get("/health", () => <div>ok</div>);
"#;
        let found = extract_entrypoints(&language("tsx"), &PathBuf::from("server.tsx"), source);
        assert!(
            found.iter().any(|e| e.route.as_deref() == Some("/health")),
            "got {found:?}"
        );
    }

    #[test]
    fn finds_python_main_guard() {
        let source = b"if __name__ == \"__main__\":\n    run()\n";
        let found = extract_entrypoints(&language("python"), &PathBuf::from("m.py"), source);
        // A main guard is how the outside world runs the script, so it is an
        // external entrypoint rather than internal startup wiring.
        let main = found
            .iter()
            .find(|e| e.name == "__main__")
            .expect("main guard found");
        assert_eq!(main.kind, EntryPointKind::Cli);
        assert_eq!(main.exposure, Exposure::External);
    }

    #[test]
    fn an_unfamiliar_receiver_name_still_registers_a_route() {
        // The worst failure this tool can have is a silently missed endpoint,
        // because everything downstream depends on knowing the ways in. The
        // route shape plus a handler is the evidence; the object's name is not.
        let source = br#"
myBackend.get("/users", listUsers);
srv2.post("/orders", createOrder);
"#;
        let found = extract_entrypoints(&language("javascript"), &PathBuf::from("s.js"), source);
        let routes: Vec<_> = found.iter().filter_map(|e| e.route.as_deref()).collect();

        assert!(routes.contains(&"/users"), "got {found:?}");
        assert!(routes.contains(&"/orders"), "got {found:?}");
    }

    #[test]
    fn map_operations_are_not_routes() {
        let source = br#"
const cache = new Map();
cache.get("/not-a-route");
results.delete("/also-not");
"#;
        let found = extract_entrypoints(&language("javascript"), &PathBuf::from("c.js"), source);
        assert!(
            found.is_empty(),
            "non-allowlisted receivers must not produce entrypoints: {found:?}"
        );
    }

    #[test]
    fn finds_rust_attribute_routes_and_axum_builders() {
        let source = br#"
#[get("/users")]
async fn list_users() -> HttpResponse {
    HttpResponse::Ok().finish()
}

fn build() -> Router {
    Router::new().route("/health", get(health))
}

fn main() {
    build();
}
"#;
        let found = extract_entrypoints(&language("rust"), &PathBuf::from("main.rs"), source);
        let routes: Vec<_> = found.iter().filter_map(|e| e.route.as_deref()).collect();

        assert!(routes.contains(&"/users"), "actix attribute route: {found:?}");
        assert!(
            routes.contains(&"/health"),
            "axum builder route (receiver `Router::new()`): {found:?}"
        );
        let main = found.iter().find(|e| e.name == "main").expect("fn main");
        assert_eq!(main.kind, EntryPointKind::Cli);
        assert_eq!(
            main.exposure,
            Exposure::External,
            "a binary's main is invoked from outside the process"
        );
    }

    #[test]
    fn finds_csharp_attribute_routes() {
        let source = br#"
public class UsersController {
    [HttpGet("/users")]
    public IActionResult List() { return Ok(); }

    [HttpPost("/users")]
    public IActionResult Create() { return Ok(); }
}
"#;
        let found = extract_entrypoints(&language("csharp"), &PathBuf::from("Users.cs"), source);
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"GET /users"), "got {names:?}");
        assert!(names.contains(&"POST /users"), "got {names:?}");
    }

    #[test]
    fn finds_spring_annotated_routes() {
        let source = br#"
@RestController
public class UserController {
    @GetMapping("/users")
    public List<User> list() { return service.all(); }

    @Scheduled
    public void cleanup() { }
}
"#;
        let found = extract_entrypoints(&language("java"), &PathBuf::from("UserController.java"), source);
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();

        assert!(names.contains(&"GET /users"), "got {names:?}");
        assert!(
            found.iter().any(|e| e.kind == EntryPointKind::Scheduled),
            "@Scheduled should register a scheduled entrypoint: {found:?}"
        );
        assert_eq!(
            found
                .iter()
                .find(|e| e.name == "GET /users")
                .and_then(|e| e.handler_name.as_deref()),
            Some("list")
        );
    }

    #[test]
    fn finds_sinatra_bare_routes() {
        let source = br#"
get "/users" do
  User.all.to_json
end

post "/users" do
  User.create
end
"#;
        let found = extract_entrypoints(&language("ruby"), &PathBuf::from("app.rb"), source);
        let routes: Vec<_> = found.iter().filter_map(|e| e.route.as_deref()).collect();
        assert!(routes.contains(&"/users"), "got {found:?}");
        assert_eq!(
            found
                .iter()
                .filter(|e| e.kind == EntryPointKind::Http)
                .count(),
            2,
            "expected both Sinatra routes: {found:?}"
        );
    }

    #[test]
    fn bare_calls_without_a_route_are_rejected() {
        // A bare `run()` has no receiver and no route, so there is no evidence
        // it starts anything. It must not become a startup entrypoint.
        let source = br#"
run();
listen(3000);
"#;
        let found = extract_entrypoints(&language("javascript"), &PathBuf::from("x.js"), source);
        assert!(
            found.iter().all(|e| e.kind != EntryPointKind::Startup),
            "bare run()/listen() must not be startup entrypoints: {found:?}"
        );
    }

    #[test]
    fn finds_go_handlers() {
        let source = br#"
package main

func main() {
	http.HandleFunc("/health", healthHandler)
	http.ListenAndServe(":8080", nil)
}
"#;
        let found = extract_entrypoints(&language("go"), &PathBuf::from("main.go"), source);
        assert!(
            found.iter().any(|e| e.route.as_deref() == Some("/health")),
            "got {found:?}"
        );
        assert!(found.iter().any(|e| e.kind == EntryPointKind::Startup));
    }
}

/// The identifier inside a C or C++ declarator chain.
///
/// `int main(void)` nests the name under `declarator`, sometimes through a
/// pointer or parenthesised declarator, so the chain is followed rather than
/// assumed to be one level deep.
fn declarator_identifier<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let mut current = node.child_by_field_name("declarator")?;
    for _ in 0..6 {
        if current.kind() == "identifier" || current.kind() == "field_identifier" {
            return Some(current);
        }
        match current.child_by_field_name("declarator") {
            Some(next) => current = next,
            None => return None,
        }
    }
    None
}

/// Whether a call throws its return value away.
///
/// Registering a route is a statement — nothing reads the result. A client
/// request is always consumed: awaited, assigned, returned, or chained into
/// `.then`. That difference is what tells `myBackend.get("/users", handler)`
/// apart from `api.get("/users", config)` when the receiver name is no help.
fn result_is_discarded(site: Node<'_>) -> bool {
    let mut current = site;
    for _ in 0..4 {
        let Some(parent) = current.parent() else {
            return false;
        };
        match parent.kind() {
            // Statement position: the value goes nowhere.
            "expression_statement" | "block" | "program" | "statement_block" => return true,
            // Wrappers that do not themselves consume the value; keep walking.
            "parenthesized_expression" => current = parent,
            // Anything else - assignment, await, return, argument, member
            // access, chained `.then` - uses the result.
            _ => return false,
        }
    }
    false
}

/// The first identifier-like child of a node.
///
/// Kotlin and Swift declare `fun main()` with the name as a plain
/// `simple_identifier` rather than a named field.
fn first_identifier_child<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|child| matches!(child.kind(), "simple_identifier" | "identifier"));
    found
}
