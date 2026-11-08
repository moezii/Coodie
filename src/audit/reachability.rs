//! Reachability.
//!
//! Can the outside world get here? That question decides whether a symbol is a
//! feature, plumbing, or something nobody can account for.
//!
//! The adjacency index is built **once** and every query reuses it. Rebuilding
//! it per node, or revalidating the graph on every call, makes coverage review
//! quadratic; this does one traversal per root set and then answers in constant
//! time.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::graph::{
    external_roots, public_api_roots, startup_roots, test_roots, EdgeType, Graph, NodeKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Reachability {
    /// A directed path exists from an externally exposed entrypoint.
    Reachable,
    /// Reachable from an entrypoint, but only along a polymorphic call whose
    /// target could not be pinned down. The path is inferred, not proven.
    ReachableDynamic,
    /// Reachable only from process startup, not from outside.
    InternallyReachable,
    /// Reachable only from test code.
    TestOnly,
    /// Part of the public API, but nothing inside this repository uses it.
    ///
    /// Expected for a published library — consumers are the callers. For an
    /// application it is dead code wearing a `pub` keyword, which is why it is
    /// reported separately rather than being silently counted as reachable.
    PublicUnused,
    /// Nothing references it and nothing reaches it.
    DeadOrUnreferenced,
    /// Nothing traced references it, but its name occurs somewhere else in the
    /// repository — a macro, a build file, a config, a string, another
    /// language. That may be a use no extractor follows, so it is unresolved,
    /// never dead. See [`super::mentions`].
    MentionedNotTraced,
    /// It has incident edges but no path from any known root. Absence of a path
    /// is not proof of dead code, so this stays explicitly unresolved.
    Unknown,
}

impl Reachability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::ReachableDynamic => "reachable_via_dynamic_dispatch",
            Self::InternallyReachable => "internally_reachable",
            Self::TestOnly => "test_only",
            Self::PublicUnused => "public_but_unused_internally",
            Self::DeadOrUnreferenced => "dead_or_unreferenced",
            Self::MentionedNotTraced => "mentioned_but_not_traced",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReachabilityIndex {
    status: HashMap<String, Reachability>,
    /// Everything a test can reach. Kept separately from `status` because a
    /// symbol reached by both production code and a test is `Reachable`, and we
    /// still need to know a test exercises it.
    test_reachable: HashSet<String>,
}

impl ReachabilityIndex {
    pub fn build(graph: &Graph) -> Self {
        // Two adjacency views: one over proven call edges only, one that also
        // follows polymorphic dispatch. The difference is what separates
        // "proven reachable" from "reachable if this dispatch goes that way".
        let proven = adjacency(graph, false);
        let inferred = adjacency(graph, true);

        let external_proven = traverse(
            external_roots(graph).iter().map(|node| node.id.clone()),
            &proven,
        );
        let external = traverse(
            external_roots(graph).iter().map(|node| node.id.clone()),
            &inferred,
        );
        // A library has no routes; its exports are how the outside world gets
        // in. Treating the public API as roots is what lets one engine judge a
        // package, a CLI, and a web service without being told which it is.
        let public_proven = traverse(
            public_api_roots(graph).iter().map(|node| node.id.clone()),
            &proven,
        );
        let public = traverse(
            public_api_roots(graph).iter().map(|node| node.id.clone()),
            &inferred,
        );
        let startup = traverse(
            startup_roots(graph).iter().map(|node| node.id.clone()),
            &inferred,
        );
        let tests = traverse(
            test_roots(graph).iter().map(|node| node.id.clone()),
            &inferred,
        );

        let incident = incident_nodes(graph);
        let referenced: HashSet<&str> = graph
            .referenced_names
            .iter()
            .map(String::as_str)
            .collect();
        let dispatched: HashSet<&str> = graph.dispatched.iter().map(String::as_str).collect();
        let mentioned: HashSet<&str> = graph
            .untraced_mentions
            .iter()
            .map(|mention| mention.node_id.as_str())
            .collect();
        let mut status = HashMap::with_capacity(graph.nodes.len());

        for node in &graph.nodes {
            if node.kind == NodeKind::File {
                continue;
            }
            // A public symbol nothing else reaches is API surface, not proof of
            // use. Counting it as plainly reachable hides the case that matters:
            // an application whose `pub` is module plumbing rather than a
            // published interface.
            let reached_internally = external.contains(&node.id)
                || startup.contains(&node.id)
                || tests.contains(&node.id)
                || incident.contains(node.id.as_str())
                // Called by a name we could not pin to one definition — a trait
                // method, an override, a qualifier. Still a use.
                || is_referenced(&referenced, &node.name);
            let value = if external_proven.contains(&node.id) {
                Reachability::Reachable
            } else if external.contains(&node.id) {
                Reachability::ReachableDynamic
            } else if public_proven.contains(&node.id) || public.contains(&node.id) {
                if reached_internally {
                    Reachability::Reachable
                } else {
                    Reachability::PublicUnused
                }
            } else if startup.contains(&node.id) {
                Reachability::InternallyReachable
            } else if tests.contains(&node.id) {
                Reachability::TestOnly
            } else if incident.contains(node.id.as_str())
                || is_referenced(&referenced, &node.name)
                || dispatched.contains(node.id.as_str())
                || is_nested_type(node)
            {
                // Something references it — either by a resolved edge, or by a
                // call to its name that we could not pin to a definition (trait
                // impls, interface methods, virtual overrides). We cannot prove
                // it is dead, so we refuse to say that it is.
                Reachability::Unknown
            } else if mentioned.contains(node.id.as_str()) {
                Reachability::MentionedNotTraced
            } else {
                Reachability::DeadOrUnreferenced
            };
            status.insert(node.id.clone(), value);
        }

        Self {
            status,
            test_reachable: tests,
        }
    }

    /// Ids of every symbol currently classified dead, in no particular order.
    pub fn dead_ids(&self) -> impl Iterator<Item = &str> {
        self.status
            .iter()
            .filter(|(_, status)| **status == Reachability::DeadOrUnreferenced)
            .map(|(id, _)| id.as_str())
    }

    /// Apply the graph's [`Graph::untraced_mentions`] to an index built before
    /// they were known, without rebuilding it. Gives the same answer as
    /// [`ReachabilityIndex::build`] on the updated graph.
    pub fn apply_untraced_mentions(&mut self, graph: &Graph) {
        for mention in &graph.untraced_mentions {
            if let Some(status) = self.status.get_mut(&mention.node_id) {
                if *status == Reachability::DeadOrUnreferenced {
                    *status = Reachability::MentionedNotTraced;
                }
            }
        }
    }

    /// Does any test reach this symbol?
    pub fn reached_by_tests(&self, node_id: &str) -> bool {
        self.test_reachable.contains(node_id)
    }

    pub fn status(&self, node_id: &str) -> Reachability {
        self.status
            .get(node_id)
            .copied()
            .unwrap_or(Reachability::Unknown)
    }

    pub fn count(&self, value: Reachability) -> usize {
        self.status
            .values()
            .filter(|status| **status == value)
            .count()
    }
}

/// Is this name used somewhere we could not resolve to one definition?
///
/// A C++ destructor `~Foo` is never named at a call site: it runs whenever a
/// `Foo` dies, so any use of `Foo` is a use of it.
fn is_referenced(referenced: &HashSet<&str>, name: &str) -> bool {
    referenced.contains(name)
        || name
            .strip_prefix('~')
            .is_some_and(|class| referenced.contains(class.trim()))
}

/// A type declared inside another type is reached through its parent, never by
/// its bare name. Pydantic's `class Config` and Django's `class Meta` are the
/// common cases — framework-read, never referenced, and not dead.
fn is_nested_type(node: &super::graph::GraphNode) -> bool {
    node.symbol_kind == Some(crate::index::SymbolKind::Class)
        && node.name.chars().next().is_some_and(char::is_uppercase)
        && matches!(node.name.as_str(), "Config" | "Meta" | "Params" | "Arguments")
}

/// Directed adjacency over meaningful edges. `Contains` is excluded: a file
/// holding a function does not mean anything calls it.
///
/// With `include_dynamic`, polymorphic dispatch edges are followed too.
fn adjacency(graph: &Graph, include_dynamic: bool) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &graph.edges {
        if edge.edge_type == EdgeType::Contains {
            continue;
        }
        if edge.edge_type == EdgeType::DynamicCalls && !include_dynamic {
            continue;
        }
        map.entry(edge.source.as_str())
            .or_default()
            .push(edge.target.as_str());
    }
    map
}

/// Every node touched by a meaningful edge, in either direction.
fn incident_nodes(graph: &Graph) -> HashSet<&str> {
    let mut set = HashSet::new();
    for edge in &graph.edges {
        if edge.edge_type == EdgeType::Contains {
            continue;
        }
        set.insert(edge.source.as_str());
        set.insert(edge.target.as_str());
    }
    set
}

/// Breadth-first traversal from a set of roots. One pass per root set.
///
/// The walk runs over borrowed ids; each reached id is copied once, into the
/// returned set.
fn traverse(
    roots: impl Iterator<Item = String>,
    adjacency: &HashMap<&str, Vec<&str>>,
) -> HashSet<String> {
    let roots: Vec<String> = roots.collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();

    for root in &roots {
        if seen.insert(root.as_str()) {
            queue.push_back(root.as_str());
        }
    }

    while let Some(current) = queue.pop_front() {
        let Some(neighbours) = adjacency.get(current) else {
            continue;
        };
        for neighbour in neighbours {
            if seen.insert(neighbour) {
                queue.push_back(neighbour);
            }
        }
    }

    seen.into_iter().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::graph::{Edge, GraphNode};
    use crate::audit::{EntryPointKind, Exposure};
    use crate::index::LineRange;

    fn function(id: &str, file: &str) -> GraphNode {
        GraphNode {
            id: id.to_owned(),
            kind: NodeKind::Function,
            name: id.to_owned(),
            file: file.to_owned(),
            line_range: LineRange { start: 1, end: 2 },
            language: "javascript".to_owned(),
            entrypoint_kind: None,
            exposure: None,
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        }
    }

    fn entrypoint(id: &str, exposure: Exposure) -> GraphNode {
        GraphNode {
            id: id.to_owned(),
            kind: NodeKind::EntryPoint,
            name: id.to_owned(),
            file: "server.js".to_owned(),
            line_range: LineRange { start: 1, end: 1 },
            language: "javascript".to_owned(),
            entrypoint_kind: Some(EntryPointKind::Http),
            exposure: Some(exposure),
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
    fn traces_reachability_through_a_call_chain() {
        let graph = Graph {
            nodes: vec![
                entrypoint("ep", Exposure::External),
                function("handler", "server.js"),
                function("service", "service.js"),
                function("orphan", "orphan.js"),
            ],
            edges: vec![
                edge("ep", "handler", EdgeType::HandledBy),
                edge("handler", "service", EdgeType::Calls),
            ],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_eq!(index.status("handler"), Reachability::Reachable);
        assert_eq!(index.status("service"), Reachability::Reachable);
        assert_eq!(index.status("orphan"), Reachability::DeadOrUnreferenced);
    }

    #[test]
    fn startup_only_code_is_not_externally_reachable() {
        let graph = Graph {
            nodes: vec![
                entrypoint("boot", Exposure::Startup),
                function("configure", "boot.js"),
            ],
            edges: vec![edge("boot", "configure", EdgeType::Calls)],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_eq!(
            index.status("configure"),
            Reachability::InternallyReachable
        );
    }

    #[test]
    fn code_reached_only_by_tests_is_test_only() {
        let graph = Graph {
            nodes: vec![
                function("login.test", "src/login.test.js"),
                function("login", "src/login.js"),
            ],
            edges: vec![edge("login.test", "login", EdgeType::Calls)],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_eq!(index.status("login"), Reachability::TestOnly);
    }

    #[test]
    fn polymorphic_dispatch_is_reachable_but_flagged_as_inferred() {
        // `provider.complete()` may land on either impl. Both are reachable,
        // but the path is inferred, so neither may claim to be proven.
        let graph = Graph {
            nodes: vec![
                entrypoint("ep", Exposure::External),
                function("run", "src/run.rs"),
                function("openai_complete", "src/llm/openai.rs"),
                function("anthropic_complete", "src/llm/anthropic.rs"),
            ],
            edges: vec![
                edge("ep", "run", EdgeType::HandledBy),
                edge("run", "openai_complete", EdgeType::DynamicCalls),
                edge("run", "anthropic_complete", EdgeType::DynamicCalls),
            ],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_eq!(index.status("run"), Reachability::Reachable);
        assert_eq!(
            index.status("openai_complete"),
            Reachability::ReachableDynamic
        );
        assert_eq!(
            index.status("anthropic_complete"),
            Reachability::ReachableDynamic
        );
    }

    #[test]
    fn a_proven_call_outranks_an_inferred_one() {
        let graph = Graph {
            nodes: vec![
                entrypoint("ep", Exposure::External),
                function("target", "src/a.rs"),
            ],
            edges: vec![
                edge("ep", "target", EdgeType::DynamicCalls),
                edge("ep", "target", EdgeType::Calls),
            ],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_eq!(index.status("target"), Reachability::Reachable);
    }

    #[test]
    fn a_public_symbol_nothing_uses_is_named_rather_than_hidden() {
        // `pub` means two things: a published interface, and "the file next
        // door needs this". Counting every `pub` as reachable hides the second
        // case entirely, which is how six unused functions survived in this
        // repository until the audit was asked about them.
        let mut exported = function("exported", "src/api.rs");
        exported.is_public = true;
        exported.language = "rust".to_owned();
        let mut used = function("used", "src/api.rs");
        used.is_public = true;
        used.language = "rust".to_owned();
        let mut caller = function("caller", "src/main.rs");
        caller.is_public = true;
        caller.language = "rust".to_owned();

        let graph = Graph {
            nodes: vec![exported, used, caller],
            edges: vec![edge("caller", "used", EdgeType::Calls)],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_eq!(index.status("exported"), Reachability::PublicUnused);
        // Something calls it, so it is plainly reachable.
        assert_eq!(index.status("used"), Reachability::Reachable);
    }

    #[test]
    fn a_public_symbol_called_only_by_name_is_not_reported() {
        // Trait methods and overrides are called through a name we cannot pin
        // to one definition. That is still a use.
        let mut exported = function("fmt", "src/display.rs");
        exported.is_public = true;
        exported.language = "rust".to_owned();

        let graph = Graph {
            nodes: vec![exported],
            edges: Vec::new(),
            referenced_names: vec!["fmt".to_owned()],
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_ne!(index.status("fmt"), Reachability::PublicUnused);
    }

    #[test]
    fn trait_impls_called_by_name_are_not_reported_as_dead() {
        // `complete` is implemented twice and dispatched through a trait
        // object, so neither impl has a resolved incoming edge. Reporting them
        // as dead code would be a false accusation about the customer's code.
        let graph = Graph {
            nodes: vec![
                function("openai_complete", "src/llm/openai.rs"),
                function("anthropic_complete", "src/llm/anthropic.rs"),
            ],
            edges: Vec::new(),
            referenced_names: vec!["openai_complete".to_owned()],
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_eq!(index.status("openai_complete"), Reachability::Unknown);
        // Nothing calls this name anywhere, so it stays a genuine finding.
        assert_eq!(
            index.status("anthropic_complete"),
            Reachability::DeadOrUnreferenced
        );
    }

    #[test]
    fn referenced_but_unrooted_code_stays_unknown_not_dead() {
        let graph = Graph {
            nodes: vec![function("a", "a.js"), function("b", "b.js")],
            edges: vec![edge("a", "b", EdgeType::Calls)],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        // `b` is referenced by `a`, but nothing roots `a`. Calling that dead
        // would be a claim the graph cannot support.
        assert_eq!(index.status("b"), Reachability::Unknown);
    }

    #[test]
    fn cycles_terminate() {
        let graph = Graph {
            nodes: vec![
                entrypoint("ep", Exposure::External),
                function("a", "a.js"),
                function("b", "b.js"),
            ],
            edges: vec![
                edge("ep", "a", EdgeType::HandledBy),
                edge("a", "b", EdgeType::Calls),
                edge("b", "a", EdgeType::Calls),
            ],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let index = ReachabilityIndex::build(&graph);
        assert_eq!(index.status("a"), Reachability::Reachable);
        assert_eq!(index.status("b"), Reachability::Reachable);
    }
}
