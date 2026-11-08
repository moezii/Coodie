//! The evidence graph.
//!
//! Every node carries a file and a line range, so every downstream claim can be
//! checked against real source. Edges are only created from syntax that actually
//! appeared — a call edge means a call site exists, not that one plausibly might.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{is_test_path, EntryPoint, EntryPointKind, Exposure, IndexedFile};
use crate::index::{LineRange, Symbol, SymbolKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    File,
    Function,
    EntryPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EdgeType {
    /// File contains a symbol or entrypoint.
    Contains,
    /// One symbol calls another. A call site was proven to target it.
    Calls,
    /// A polymorphic call that may reach this symbol.
    ///
    /// `provider.complete()` on a trait object dispatches to one implementation,
    /// and static analysis cannot say which. Rather than drop the call — which
    /// would strand every implementation as unexplained — an edge is drawn to
    /// each candidate. The relationship is real (the call site exists and one of
    /// these runs); the specific target is inferred, so it is kept separate from
    /// `Calls` and reported as inferred rather than proven.
    DynamicCalls,
    /// An entrypoint is served by a handler symbol.
    HandledBy,
}

/// The largest number of implementations a single polymorphic call may point
/// at. Beyond this the name is too common for the inference to mean anything.
const MAX_DYNAMIC_TARGETS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub file: String,
    pub line_range: LineRange,
    pub language: String,
    /// Present on entrypoint nodes only.
    pub entrypoint_kind: Option<EntryPointKind>,
    /// Present on entrypoint nodes only.
    pub exposure: Option<Exposure>,
    /// Declared inside a test scope even though the file is not a test file.
    /// Rust's `#[cfg(test)] mod tests` is the common case.
    #[serde(default)]
    pub in_test_scope: bool,
    /// Whether this node is a function or a named type. Present on symbol
    /// nodes only — it lets the report say "8 unused types" rather than
    /// lumping models in with functions.
    #[serde(default)]
    pub symbol_kind: Option<SymbolKind>,
    /// Part of the public API. For a library this is what an entrypoint is to
    /// a service: the way the outside world gets in.
    #[serde(default)]
    pub is_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub edge_type: EdgeType,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<Edge>,
    /// Every callee name seen anywhere, including calls that could not be
    /// resolved to a definition.
    ///
    /// This is what separates "nothing calls this" from "something calls a
    /// method with this name and we could not prove which one". Trait impls,
    /// interface methods, and virtual overrides all land in the second bucket,
    /// and must never be reported as dead code.
    #[serde(default)]
    pub referenced_names: Vec<String>,
    /// Methods invoked through a trait, sorted: Rust `impl Trait for Type`
    /// methods. Whether or not any visible call reaches them, they can be
    /// called through the trait, so they are never reported as dead.
    #[serde(default)]
    pub dispatched: Vec<String>,
    /// Functions that make at least one call no definition in this repository
    /// answers, sorted. Code that cannot be seen may assert or panic, so a test
    /// that reaches one of these is never reported as unable to fail.
    #[serde(default)]
    pub opaque_callers: Vec<String>,
    /// Symbols nothing traced reaches whose name nevertheless occurs somewhere
    /// else in the repository, in node order. Set by the final
    /// language-independent check (see [`super::mentions`]); such a symbol is
    /// unresolved, never dead.
    #[serde(default)]
    pub untraced_mentions: Vec<super::mentions::UntracedMention>,
}

impl Graph {
    pub fn node(&self, id: &str) -> Option<&GraphNode> {
        self.index().get(id).and_then(|index| self.nodes.get(*index))
    }

    fn index(&self) -> HashMap<&str, usize> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id.as_str(), index))
            .collect()
    }

    pub fn entrypoints(&self) -> impl Iterator<Item = &GraphNode> {
        self.nodes
            .iter()
            .filter(|node| node.kind == NodeKind::EntryPoint)
    }
}

/// A call site recovered from the AST, before resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSite {
    /// The caller symbol's name, if the call sits inside a known function.
    pub caller: Option<String>,
    /// The trailing segment of the callee expression (`utils.hash` -> `hash`).
    pub callee: String,
    /// The segment before the callee (`cli::run` -> `cli`), used to break ties
    /// when several files define the same function name.
    pub qualifier: Option<String>,
    /// The scope the *caller* is declared in — the `impl` block or class. A
    /// `self.foo()` call resolves inside this scope, which is the only way to
    /// tell four different `model()` methods apart.
    pub caller_scope: Option<String>,
    pub file: String,
    /// A use by name that is not a call: a type named in an annotation, a
    /// function named in an attribute string. It links the graph like a call,
    /// but executes nothing, so it can never make a test fail.
    #[serde(default)]
    pub is_reference: bool,
}

/// The caller name given to calls made from Python's module-level
/// `if __name__ == "__main__":` block, which lives outside every function.
/// Such calls are attributed to that file's `__main__` entrypoint.
pub const MODULE_MAIN_CALLER: &str = "__main__";

/// A method defined in an `impl Trait for Type` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchedMethod {
    /// The implementing type, when it is a plain named type.
    pub type_name: Option<String>,
    /// The method's full node id.
    pub method_id: String,
}

/// Facts extracted from one file. Produced by a single parse.
///
/// Serialisable so the whole result of parsing a file can be cached against
/// that file's content hash. Caching only the symbol list was not enough: an
/// audit also needs the entrypoints, call sites, macro-visible names, assertion
/// evidence and docstrings, and without them it reparsed every file on every
/// run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileFacts {
    pub symbols: Vec<Symbol>,
    pub entrypoints: Vec<EntryPoint>,
    pub calls: Vec<CallSite>,
    /// Names that appear somewhere a call cannot be proven — inside macro
    /// bodies, most importantly. `writeln!(w, "{}", plural(n))` contains a real
    /// call to `plural`, but tree-sitter sees an opaque token tree, so the call
    /// is invisible. Recording the name keeps `plural` out of the dead-code
    /// findings without inventing an edge that was never proven.
    pub mentioned_names: Vec<String>,
    /// Symbols proven to contain an assertion. A test missing from this list
    /// checks nothing and can never fail.
    pub asserting_symbols: Vec<String>,
    /// Docstrings attached to definitions in this file.
    pub docstrings: Vec<super::claims::Claim>,
    /// Test cases a framework construct declares (`TEST(Suite, Name)`), as
    /// opposed to ones recognised by their name.
    #[serde(default)]
    pub declared_tests: Vec<super::testing::DeclaredTest>,
    /// Methods that implement a trait.
    #[serde(default)]
    pub dispatched: Vec<DispatchedMethod>,
    /// Node ids of functions the language marks as tests by attribute
    /// (Rust `#[test]`, `#[tokio::test]`, ...).
    #[serde(default)]
    pub test_functions: Vec<String>,
    /// `(name, line)` for every place this file *declares* a function's name
    /// rather than using it: C/C++ prototypes, out-of-class definitions and
    /// in-class method declarations. The language-independent mention gate
    /// (see [`super::mentions`]) does not count these as uses.
    #[serde(default)]
    pub declaration_sites: Vec<(String, usize)>,
}

pub fn file_id(path: &str) -> String {
    format!("file:{path}")
}

pub fn function_id(path: &str, name: &str, line: usize) -> String {
    format!("function:{path}:{name}:{line}")
}

/// A line-less id prefix, used to match a symbol by file and name before its
/// line is known.
pub fn function_id_for(path: &str, name: &str) -> String {
    format!("function:{path}:{name}")
}

/// Build the graph from indexed files, symbols, entrypoints, and call sites.
///
/// Call resolution is deliberately conservative: a call edge is only created
/// when the callee name resolves to exactly one candidate (preferring the same
/// file). An ambiguous name produces no edge rather than a guessed one — a
/// wrong edge would make dead code look reachable, which is the one mistake
/// this engine must never make.
pub fn build(
    files: &[IndexedFile],
    symbols: &[Symbol],
    entrypoints: &[EntryPoint],
    calls: &[CallSite],
) -> Graph {
    build_with_mentions(files, symbols, entrypoints, calls, &[], &[])
}

pub fn build_with_mentions(
    files: &[IndexedFile],
    symbols: &[Symbol],
    entrypoints: &[EntryPoint],
    calls: &[CallSite],
    mentioned_names: &[String],
    dispatched: &[DispatchedMethod],
) -> Graph {
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    let language_by_file: BTreeMap<&str, &str> = files
        .iter()
        .map(|file| (file.relative.as_str(), file.language.id()))
        .collect();

    for file in files {
        nodes.push(GraphNode {
            id: file_id(&file.relative),
            kind: NodeKind::File,
            name: file.relative.clone(),
            file: file.relative.clone(),
            line_range: LineRange {
                start: 1,
                end: file.line_count.max(1),
            },
            language: file.language.id().to_owned(),
            entrypoint_kind: None,
            exposure: None,
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        });
    }

    // Symbol nodes, plus the lookups used to resolve calls. Several indexes,
    // because a name alone is rarely enough: `model()` is defined once per
    // provider `impl`, so only the scope tells them apart.
    //
    // Each symbol's normalised path and node id are computed once, and the
    // lookups borrow from them instead of each holding its own copies. That
    // also lets call resolution look keys up without allocating them.
    let symbol_paths: Vec<String> = symbols
        .iter()
        .map(|symbol| symbol.file.to_string_lossy().replace('\\', "/"))
        .collect();
    let symbol_ids: Vec<String> = symbols
        .iter()
        .zip(&symbol_paths)
        .map(|(symbol, path)| function_id(path, &symbol.name, symbol.line_range.start))
        .collect();
    let mut ids_by_name: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut ids_by_file_and_name: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
    let mut ids_by_file_scope_name: HashMap<(&str, &str, &str), &str> = HashMap::new();
    let mut ids_by_scope_and_name: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
    let mut scope_by_id: HashMap<&str, &str> = HashMap::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for ((symbol, path), id) in symbols.iter().zip(&symbol_paths).zip(&symbol_ids) {
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let language = language_by_file
            .get(path.as_str())
            .copied()
            .unwrap_or("unknown");

        nodes.push(GraphNode {
            id: id.clone(),
            kind: NodeKind::Function,
            name: symbol.name.clone(),
            line_range: symbol.line_range,
            file: path.clone(),
            in_test_scope: is_test_scope(&symbol.enclosing_scope),
            is_public: symbol.is_public,
            language: language.to_owned(),
            entrypoint_kind: None,
            exposure: None,
            symbol_kind: Some(symbol.kind),
        });
        edges.push(Edge {
            source: file_id(path),
            target: id.clone(),
            edge_type: EdgeType::Contains,
        });

        let name = symbol.name.as_str();
        let scope = symbol.enclosing_scope.as_str();
        scope_by_id.insert(id, scope);
        ids_by_name.entry(name).or_default().push(id);
        ids_by_file_and_name.entry((path, name)).or_default().push(id);
        if !scope.is_empty() {
            ids_by_file_scope_name
                .entry((path, scope, name))
                .or_insert(id);
            ids_by_scope_and_name
                .entry((scope, name))
                .or_default()
                .push(id);
        }
    }

    // Layer 3 of call resolution asks which same-named symbols live in a file
    // whose stem or directory equals the qualifier. Answering that by scanning
    // every candidate made each call cost O(symbols sharing its name), which is
    // quadratic on common names like `new`. Index each id under every path
    // token it would match instead; a lookup then gives the same set.
    let mut ids_by_name_and_path_token: HashMap<&str, HashMap<String, Vec<&str>>> =
        HashMap::new();
    for (name, ids) in &ids_by_name {
        let by_token = ids_by_name_and_path_token.entry(name).or_default();
        for id in ids {
            for token in path_tokens(id) {
                by_token.entry(token).or_default().push(id);
            }
        }
    }

    let index = SymbolIndex {
        by_name: &ids_by_name,
        by_name_and_path_token: &ids_by_name_and_path_token,
        by_file_and_name: &ids_by_file_and_name,
        by_file_scope_name: &ids_by_file_scope_name,
        by_scope_and_name: &ids_by_scope_and_name,
        scope_by_id: &scope_by_id,
    };

    // Python `__main__` entrypoints by file, the source of calls made from the
    // module-level main block.
    let mut module_main_by_file: HashMap<String, String> = HashMap::new();

    // Entrypoint nodes, wired to their handler when the handler is unambiguous.
    for entrypoint in entrypoints {
        let path = entrypoint.file.to_string_lossy().replace('\\', "/");
        let id = entrypoint.id(&path);
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let language = language_by_file
            .get(path.as_str())
            .copied()
            .unwrap_or("unknown");

        nodes.push(GraphNode {
            id: id.clone(),
            kind: NodeKind::EntryPoint,
            name: entrypoint.name.clone(),
            file: path.clone(),
            line_range: entrypoint.line_range,
            language: language.to_owned(),
            entrypoint_kind: Some(entrypoint.kind),
            exposure: Some(entrypoint.exposure),
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        });
        edges.push(Edge {
            source: file_id(&path),
            target: id.clone(),
            edge_type: EdgeType::Contains,
        });
        if entrypoint.name == MODULE_MAIN_CALLER && entrypoint.handler_name.is_none() {
            module_main_by_file
                .entry(path.clone())
                .or_insert_with(|| id.clone());
        }

        if let Some(handler) = &entrypoint.handler_name {
            if let Some(target) = resolve_call(handler, None, &path, None, &index) {
                edges.push(Edge {
                    source: id.clone(),
                    target,
                    edge_type: EdgeType::HandledBy,
                });
            }
        }
    }

    // Call edges.
    let mut seen_edges: HashSet<(String, String, EdgeType)> = edges
        .iter()
        .map(|edge| (edge.source.clone(), edge.target.clone(), edge.edge_type))
        .collect();
    let mut opaque_callers: HashSet<String> = HashSet::new();

    for call in calls {
        let Some(caller) = &call.caller else { continue };
        // The caller is identified by its own scope when it has one, so two
        // same-named methods in one file stay distinct.
        let source_id: &str = match call
            .caller_scope
            .as_ref()
            .filter(|scope| !scope.is_empty())
            .and_then(|scope| {
                ids_by_file_scope_name.get(&(
                    call.file.as_str(),
                    scope.as_str(),
                    caller.as_str(),
                ))
            }) {
            Some(id) => id,
            None => match ids_by_file_and_name.get(&(call.file.as_str(), caller.as_str())) {
                Some(candidates) if candidates.len() == 1 => {
                    candidates.first().expect("checked length")
                }
                _ if caller == MODULE_MAIN_CALLER => match module_main_by_file.get(&call.file) {
                    Some(id) => id,
                    None => continue,
                },
                _ => continue,
            },
        };
        let resolved = resolve_call(
            &call.callee,
            call.qualifier.as_deref(),
            &call.file,
            call.caller_scope.as_deref(),
            &index,
        );

        let (targets, edge_type) = match resolved {
            Some(target) => (vec![target], EdgeType::Calls),
            // Unresolved: if every candidate is a method on some type, this is
            // polymorphic dispatch rather than a missing definition.
            None => match dynamic_targets(&call.callee, call.qualifier.as_deref(), &index) {
                Some(candidates) => (candidates, EdgeType::DynamicCalls),
                None => {
                    if !call.is_reference {
                        opaque_callers.insert(source_id.to_string());
                    }
                    continue;
                }
            },
        };

        for target_id in targets {
            if source_id == target_id {
                continue;
            }
            let key = (source_id.to_owned(), target_id.clone(), edge_type);
            if !seen_edges.insert(key) {
                continue;
            }
            edges.push(Edge {
                source: source_id.to_owned(),
                target: target_id,
                edge_type,
            });
        }
    }

    // A trait method runs whenever its type is used through the trait, so it is
    // reachable exactly when its type is. The edge is inferred, like any
    // dispatch, and is drawn only when the type resolves to one definition.
    let mut dispatched_ids: Vec<String> = Vec::with_capacity(dispatched.len());
    for method in dispatched {
        dispatched_ids.push(method.method_id.clone());
        let Some(type_name) = &method.type_name else { continue };
        let file = method
            .method_id
            .strip_prefix("function:")
            .and_then(|rest| rest.split(':').next())
            .unwrap_or("");
        let Some(type_id) = resolve_call(type_name, None, file, None, &index) else {
            continue;
        };
        if type_id == method.method_id {
            continue;
        }
        let key = (type_id.clone(), method.method_id.clone(), EdgeType::DynamicCalls);
        if seen_edges.insert(key) {
            edges.push(Edge {
                source: type_id,
                target: method.method_id.clone(),
                edge_type: EdgeType::DynamicCalls,
            });
        }
    }
    dispatched_ids.sort();
    dispatched_ids.dedup();

    // Node ids and (source, target, type) triples are unique by construction
    // (`seen_ids`, `seen_edges`), so an unstable sort gives the same order.
    nodes.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    edges.sort_unstable_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then(left.target.cmp(&right.target))
            .then(left.edge_type.cmp(&right.edge_type))
    });

    // A qualifier names something too. `CarController.get_all()` is a use of
    // `CarController`, but only `get_all` appears as the callee — so without
    // this, every controller and service class reads as unreferenced.
    // Deduplicated as borrowed strings before sorting, so only the distinct
    // names are sorted and copied.
    let referenced_names: HashSet<&str> = calls
        .iter()
        .map(|call| call.callee.as_str())
        .chain(calls.iter().filter_map(|call| call.qualifier.as_deref()))
        .chain(
            entrypoints
                .iter()
                .filter_map(|entrypoint| entrypoint.handler_name.as_deref()),
        )
        .chain(mentioned_names.iter().map(String::as_str))
        .collect();
    let mut referenced_names: Vec<&str> = referenced_names.into_iter().collect();
    referenced_names.sort_unstable();
    let referenced_names: Vec<String> = referenced_names.into_iter().map(str::to_owned).collect();

    Graph {
        nodes,
        edges,
        referenced_names,
        dispatched: dispatched_ids,
        opaque_callers: {
            let mut sorted: Vec<String> = opaque_callers.into_iter().collect();
            sorted.sort();
            sorted
        },
        untraced_mentions: Vec::new(),
    }
}

/// A symbol declared inside a test module is test code even when the file it
/// lives in is production source. Rust's `#[cfg(test)] mod tests` is the case
/// this exists for; Python's `class TestFoo` and Java's inner test classes
/// follow the same shape.
pub(crate) fn is_test_scope(enclosing_scope: &str) -> bool {
    if enclosing_scope.is_empty() {
        return false;
    }
    // Split on `_` too, so `live_tests` and `integration_tests` count. Whole
    // words only — `latest` ends in "test" and is not a test module — plus the
    // PascalCase form, since Python and Java name test classes `TestFoo`.
    enclosing_scope
        .split(['.', ':', '/', '_'])
        .any(|segment| {
            let trimmed = segment.trim();
            let lower = trimmed.to_ascii_lowercase();
            lower == "tests" || lower == "test" || trimmed.starts_with("Test")
        })
}

/// The symbol lookups call resolution needs, bundled so the resolver signature
/// stays readable.
struct SymbolIndex<'a> {
    by_name: &'a HashMap<&'a str, Vec<&'a str>>,
    /// Name -> lowercased path token (file stem or path segment) -> ids. See
    /// [`path_tokens`].
    by_name_and_path_token: &'a HashMap<&'a str, HashMap<String, Vec<&'a str>>>,
    by_file_and_name: &'a HashMap<(&'a str, &'a str), Vec<&'a str>>,
    by_file_scope_name: &'a HashMap<(&'a str, &'a str, &'a str), &'a str>,
    by_scope_and_name: &'a HashMap<(&'a str, &'a str), Vec<&'a str>>,
    /// Symbol id -> the scope it is declared in. Empty for free functions.
    scope_by_id: &'a HashMap<&'a str, &'a str>,
}

/// Candidate targets for an unresolved *method* call.
///
/// Returns `Some` only when the call has a receiver and every candidate is a
/// method on some type — the signature of dispatch through a trait, interface,
/// or base class. A bare unresolved function name gets `None`, because that is
/// ambiguity rather than polymorphism.
fn dynamic_targets(
    name: &str,
    qualifier: Option<&str>,
    index: &SymbolIndex<'_>,
) -> Option<Vec<String>> {
    let qualifier = qualifier?;
    if qualifier.is_empty() || is_self_qualifier(qualifier) {
        return None;
    }

    let candidates = index.by_name.get(name)?;
    if candidates.len() < 2 || candidates.len() > MAX_DYNAMIC_TARGETS {
        return None;
    }
    let all_methods = candidates.iter().all(|id| {
        index
            .scope_by_id
            .get(id)
            .map(|scope| !scope.is_empty())
            .unwrap_or(false)
    });
    if !all_methods {
        return None;
    }
    Some(candidates.iter().map(|id| (*id).to_owned()).collect())
}

/// Qualifiers that mean "the type I am already inside".
fn is_self_qualifier(qualifier: &str) -> bool {
    matches!(qualifier, "self" | "Self" | "this" | "cls" | "super")
}

/// Resolve a callee to exactly one symbol, using progressively weaker evidence
/// and stopping at the first layer that gives a unique answer.
///
/// The layers are ordered by how much they actually prove:
///
/// 1. `self.foo()` inside a known scope — resolves within that scope.
/// 2. `Type::foo()` — the qualifier names a scope.
/// 3. `auth_controller.foo()` — the qualifier names a module or file.
/// 4. A unique definition in the calling file.
/// 5. A globally unique name.
///
/// An explicit qualifier outranks "same file" deliberately. A FastAPI route
/// handler named `login` that calls `auth_controller.login()` would otherwise
/// resolve to *itself*, the self-edge would be dropped, and the entire
/// controller layer would be reported as unreachable.
///
/// If no layer yields exactly one candidate, no edge is created. A wrong edge
/// makes dead code look reachable, which is worse than an unresolved one.
fn resolve_call(
    name: &str,
    qualifier: Option<&str>,
    from_file: &str,
    from_scope: Option<&str>,
    index: &SymbolIndex<'_>,
) -> Option<String> {
    // 1. A self-call resolves inside the caller's own scope. This is what tells
    //    four different `model()` methods apart.
    let self_call = qualifier.is_none_or(is_self_qualifier);
    if self_call {
        if let Some(scope) = from_scope.filter(|scope| !scope.is_empty()) {
            if let Some(id) = index.by_file_scope_name.get(&(from_file, scope, name)) {
                return Some((*id).to_owned());
            }
        }
    }

    if let Some(qualifier) = qualifier.filter(|value| !value.is_empty()) {
        if !is_self_qualifier(qualifier) {
            // 2. `LlmConfig::parse()` — the qualifier names the scope.
            if let Some(candidates) = index.by_scope_and_name.get(&(qualifier, name)) {
                if candidates.len() == 1 {
                    return candidates.first().map(|id| (*id).to_owned());
                }
            }

            // 3. `auth_controller.login()` — the qualifier names the module.
            if let Some(by_token) = index.by_name_and_path_token.get(name) {
                let needle = qualifier.to_ascii_lowercase();
                if let Some(matches) = by_token.get(&needle) {
                    if matches.len() == 1 {
                        return matches.first().map(|id| (*id).to_owned());
                    }
                }
            }
        }
    }

    // 4. Unique in the calling file. Ambiguity here is real ambiguity — several
    //    `impl` blocks in one file — so it must not silently pick the first.
    if let Some(candidates) = index.by_file_and_name.get(&(from_file, name)) {
        if candidates.len() == 1 {
            return candidates.first().map(|id| (*id).to_owned());
        }
    }

    // 5. Globally unique.
    match index.by_name.get(name) {
        Some(candidates) if candidates.len() == 1 => {
            candidates.first().map(|id| (*id).to_owned())
        }
        _ => None,
    }
}

/// Every lowercased qualifier a symbol id's file path matches: its file stem
/// and each of its path segments, without duplicates. A qualifier names the
/// symbol's module exactly when it is one of these.
fn path_tokens(id: &str) -> Vec<String> {
    // ids look like `function:<path>:<name>:<line>`
    let path = id.split(':').nth(1).unwrap_or("").to_ascii_lowercase();
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("");
    let mut tokens: Vec<String> = Vec::new();
    for token in std::iter::once(stem).chain(path.split('/')) {
        if !tokens.iter().any(|seen| seen == token) {
            tokens.push(token.to_owned());
        }
    }
    tokens
}

/// Roots the outside world can enter through.
pub fn external_roots(graph: &Graph) -> Vec<&GraphNode> {
    graph
        .entrypoints()
        .filter(|node| {
            node.exposure == Some(Exposure::External) && !is_test_path(&node.file)
        })
        .collect()
}

/// The public API surface: symbols a consumer outside this repository can
/// call directly.
///
/// For a library this is what an entrypoint is to a service. Without it every
/// exported function in a library reads as unreachable, and the audit reports
/// the API as dead code.
pub fn public_api_roots(graph: &Graph) -> Vec<&GraphNode> {
    // Whether the repository already tells us how it is entered.
    let has_entrypoints = graph
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::EntryPoint && node.exposure == Some(Exposure::External));

    graph
        .nodes
        .iter()
        .filter(|node| {
            node.kind == NodeKind::Function
                && node.is_public
                && !is_test_path(&node.file)
                && !node.in_test_scope
                // An explicit `pub` or `export` is the author declaring that
                // consumers use this, and counts wherever it appears. Python
                // and Ruby have no such keyword — the absence of a leading
                // underscore is not a declaration — so their symbols only
                // become roots when nothing else tells us how the code is
                // entered. Otherwise every internal helper in a Python service
                // would look reachable and dead code would stop being findable.
                && (declares_visibility(&node.language) || !has_entrypoints)
        })
        .collect()
}

/// Does this language have a visibility keyword, as opposed to a convention?
fn declares_visibility(language: &str) -> bool {
    matches!(
        language,
        "rust" | "javascript" | "typescript" | "tsx" | "java" | "csharp" | "cpp" | "go" | "scala" | "php"
    )
}

/// Roots that only run while the process starts up.
pub fn startup_roots(graph: &Graph) -> Vec<&GraphNode> {
    graph
        .entrypoints()
        .filter(|node| node.exposure == Some(Exposure::Startup))
        .collect()
}

/// Roots inside test code. Reaching a symbol only from here means it is tested
/// but not shipped.
pub fn test_roots(graph: &Graph) -> Vec<&GraphNode> {
    graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Function && is_test_path(&node.file))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{ByteRange, SourceLanguage, SymbolKind};
    use std::path::PathBuf;

    fn symbol(file: &str, name: &str, start: usize) -> Symbol {
        Symbol {
            name: name.to_owned(),
            kind: SymbolKind::Function,
            language: SourceLanguage::new("javascript").unwrap(),
            file: PathBuf::from(file),
            byte_range: ByteRange { start: 0, end: 1 },
            line_range: LineRange {
                start,
                end: start + 3,
            },
            enclosing_scope: String::new(),
            is_public: true,
        }
    }

    fn indexed(file: &str) -> IndexedFile {
        IndexedFile {
            relative: file.to_owned(),
            language: SourceLanguage::new("javascript").unwrap(),
            line_count: 100,
            symbol_count: 1,
        }
    }

    #[test]
    fn detects_rust_style_inline_test_modules() {
        assert!(is_test_scope("tests"));
        assert!(is_test_scope("TestUserService"));
        assert!(!is_test_scope("UserService"));
        assert!(!is_test_scope(""));
        // "latest" must not read as a test scope.
        assert!(!is_test_scope("latest"));
        // Rust integration modules are commonly named this way.
        assert!(is_test_scope("live_tests"));
        assert!(is_test_scope("integration_tests"));
    }

    #[test]
    fn links_entrypoint_to_its_handler() {
        let files = vec![indexed("server.js")];
        let symbols = vec![symbol("server.js", "listUsers", 20)];
        let entrypoints = vec![EntryPoint {
            kind: EntryPointKind::Http,
            name: "GET /users".to_owned(),
            exposure: Exposure::External,
            http_method: Some("GET".to_owned()),
            route: Some("/users".to_owned()),
            handler_name: Some("listUsers".to_owned()),
            file: PathBuf::from("server.js"),
            line_range: LineRange { start: 10, end: 10 },
            framework: "http".to_owned(),
        }];

        let graph = build(&files, &symbols, &entrypoints, &[]);
        let handled: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.edge_type == EdgeType::HandledBy)
            .collect();

        assert_eq!(handled.len(), 1, "expected one HandledBy edge");
        assert!(handled[0].target.contains("listUsers"));
    }

    #[test]
    fn ambiguous_callee_produces_no_edge() {
        let files = vec![indexed("a.js"), indexed("b.js"), indexed("c.js")];
        let symbols = vec![
            symbol("a.js", "caller", 1),
            symbol("b.js", "handle", 1),
            symbol("c.js", "handle", 1),
        ];
        let calls = vec![CallSite {
            caller: Some("caller".to_owned()),
            callee: "handle".to_owned(),
            qualifier: None,
            caller_scope: None,
            is_reference: false,
            file: "a.js".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        let call_edges = graph
            .edges
            .iter()
            .filter(|edge| edge.edge_type == EdgeType::Calls)
            .count();

        // Two candidates named `handle`: guessing one would fake reachability.
        assert_eq!(call_edges, 0);
    }

    #[test]
    fn unique_cross_file_callee_resolves() {
        let files = vec![indexed("a.js"), indexed("b.js")];
        let symbols = vec![symbol("a.js", "caller", 1), symbol("b.js", "handle", 1)];
        let calls = vec![CallSite {
            caller: Some("caller".to_owned()),
            callee: "handle".to_owned(),
            qualifier: None,
            caller_scope: None,
            is_reference: false,
            file: "a.js".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.edge_type == EdgeType::Calls)
                .count(),
            1
        );
    }

    #[test]
    fn qualifier_breaks_a_tie_that_would_otherwise_sever_the_chain() {
        let files = vec![indexed("main.rs"), indexed("cli.rs"), indexed("query.rs")];
        let symbols = vec![
            symbol("main.rs", "main", 1),
            symbol("cli.rs", "run", 1),
            symbol("query.rs", "run", 1),
        ];
        // `cli::run()` — ambiguous by bare name, unambiguous with the qualifier.
        let calls = vec![CallSite {
            caller: Some("main".to_owned()),
            callee: "run".to_owned(),
            qualifier: Some("cli".to_owned()),
            caller_scope: None,
            is_reference: false,
            file: "main.rs".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == EdgeType::Calls)
            .expect("qualifier should resolve the call");
        assert!(edge.target.starts_with("function:cli.rs:run"));
    }

    #[test]
    fn a_module_qualifier_outranks_a_same_named_function_in_the_calling_file() {
        // The FastAPI shape: `routes/auth.py` defines `login`, which delegates
        // to `auth_controller.login`. Resolving to the same-file `login` would
        // make it a self-edge, drop it, and strand the whole controller layer.
        let files = vec![
            indexed("backend/routes/auth.py"),
            indexed("backend/controllers/auth_controller.py"),
        ];
        let symbols = vec![
            symbol("backend/routes/auth.py", "login", 10),
            symbol("backend/controllers/auth_controller.py", "login", 7),
        ];
        let calls = vec![CallSite {
            caller: Some("login".to_owned()),
            callee: "login".to_owned(),
            qualifier: Some("auth_controller".to_owned()),
            caller_scope: None,
            is_reference: false,
            file: "backend/routes/auth.py".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == EdgeType::Calls)
            .expect("the controller call must resolve");
        assert!(
            edge.target.starts_with("function:backend/controllers/auth_controller.py:login"),
            "expected the controller, got {}",
            edge.target
        );
    }

    #[test]
    fn self_calls_resolve_within_their_own_impl_block() {
        let files = vec![indexed("llm.rs")];
        let mut openai_model = symbol("llm.rs", "model", 10);
        openai_model.enclosing_scope = "OpenAiProvider".to_owned();
        let mut anthropic_model = symbol("llm.rs", "model", 30);
        anthropic_model.enclosing_scope = "AnthropicProvider".to_owned();
        let mut caller = symbol("llm.rs", "request", 35);
        caller.enclosing_scope = "AnthropicProvider".to_owned();

        let symbols = vec![openai_model, anthropic_model, caller];
        let calls = vec![CallSite {
            caller: Some("request".to_owned()),
            callee: "model".to_owned(),
            qualifier: Some("self".to_owned()),
            caller_scope: Some("AnthropicProvider".to_owned()),
            is_reference: false,
            file: "llm.rs".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == EdgeType::Calls)
            .expect("self-call resolves");
        // Line 30 is the Anthropic impl; line 10 would be the wrong one.
        assert!(
            edge.target.ends_with(":30"),
            "self.model() must resolve inside its own impl, got {}",
            edge.target
        );
    }

    #[test]
    fn same_file_duplicates_do_not_silently_pick_the_first() {
        let files = vec![indexed("llm.rs")];
        let mut first = symbol("llm.rs", "model", 10);
        first.enclosing_scope = "A".to_owned();
        let mut second = symbol("llm.rs", "model", 30);
        second.enclosing_scope = "B".to_owned();
        // A free function with no scope, calling an ambiguous name.
        let caller = symbol("llm.rs", "helper", 50);

        let symbols = vec![first, second, caller];
        let calls = vec![CallSite {
            caller: Some("helper".to_owned()),
            callee: "model".to_owned(),
            qualifier: None,
            caller_scope: Some(String::new()),
            is_reference: false,
            file: "llm.rs".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.edge_type == EdgeType::Calls)
                .count(),
            0,
            "two same-named methods in one file must not resolve by insertion order"
        );
    }

    #[test]
    fn unresolved_method_calls_become_inferred_dispatch_edges() {
        let files = vec![indexed("main.rs"), indexed("a.rs"), indexed("b.rs")];
        let caller = symbol("main.rs", "run", 1);
        let mut impl_a = symbol("a.rs", "complete", 5);
        impl_a.enclosing_scope = "OpenAi".to_owned();
        let mut impl_b = symbol("b.rs", "complete", 5);
        impl_b.enclosing_scope = "Anthropic".to_owned();

        let symbols = vec![caller, impl_a, impl_b];
        let calls = vec![CallSite {
            caller: Some("run".to_owned()),
            callee: "complete".to_owned(),
            qualifier: Some("provider".to_owned()),
            caller_scope: Some(String::new()),
            is_reference: false,
            file: "main.rs".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        let dynamic: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| edge.edge_type == EdgeType::DynamicCalls)
            .collect();

        assert_eq!(dynamic.len(), 2, "both impls are candidates: {dynamic:?}");
        assert!(
            graph
                .edges
                .iter()
                .all(|edge| edge.edge_type != EdgeType::Calls),
            "an inferred target must never be recorded as a proven call"
        );
    }

    #[test]
    fn ambiguous_free_functions_stay_unresolved_rather_than_inferred() {
        // Free functions are not polymorphic dispatch — two unrelated helpers
        // sharing a name is plain ambiguity, and guessing would be wrong.
        let files = vec![indexed("m.rs"), indexed("a.rs"), indexed("b.rs")];
        let symbols = vec![
            symbol("m.rs", "caller", 1),
            symbol("a.rs", "handle", 1),
            symbol("b.rs", "handle", 1),
        ];
        let calls = vec![CallSite {
            caller: Some("caller".to_owned()),
            callee: "handle".to_owned(),
            qualifier: Some("thing".to_owned()),
            caller_scope: None,
            is_reference: false,
            file: "m.rs".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        assert!(
            graph.edges.iter().all(|edge| {
                edge.edge_type != EdgeType::Calls && edge.edge_type != EdgeType::DynamicCalls
            }),
            "no edge should be produced: {:?}",
            graph.edges
        );
    }

    #[test]
    fn a_qualifier_matching_several_files_still_refuses_to_guess() {
        let files = vec![indexed("a/run.rs"), indexed("b/run.rs"), indexed("m.rs")];
        let symbols = vec![
            symbol("m.rs", "caller", 1),
            symbol("a/run.rs", "go", 1),
            symbol("b/run.rs", "go", 1),
        ];
        let calls = vec![CallSite {
            caller: Some("caller".to_owned()),
            callee: "go".to_owned(),
            qualifier: Some("run".to_owned()),
            caller_scope: None,
            is_reference: false,
            file: "m.rs".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.edge_type == EdgeType::Calls)
                .count(),
            0
        );
    }

    #[test]
    fn same_file_callee_wins_over_cross_file() {
        let files = vec![indexed("a.js"), indexed("b.js")];
        let symbols = vec![
            symbol("a.js", "caller", 1),
            symbol("a.js", "handle", 10),
            symbol("b.js", "handle", 1),
        ];
        let calls = vec![CallSite {
            caller: Some("caller".to_owned()),
            callee: "handle".to_owned(),
            qualifier: None,
            caller_scope: None,
            is_reference: false,
            file: "a.js".to_owned(),
        }];

        let graph = build(&files, &symbols, &[], &calls);
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.edge_type == EdgeType::Calls)
            .expect("call edge");
        assert!(edge.target.starts_with("function:a.js:handle"));
    }
}
