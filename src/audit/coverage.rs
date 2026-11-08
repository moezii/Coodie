//! Coverage accounting.
//!
//! This is the part that matters. Not "is any claim wrong" but "how much of this
//! repository can nobody account for". Unexplained code is where the bodies are
//! buried: a wrong claim is visible and arguable, forty unreachable exported
//! functions are unknown-unknowns.
//!
//! Every meaningful node gets classified from graph evidence alone. A node is
//! *accounted for* when its classification explains why it exists — a test, a
//! config file, generated output, or something reachable from a real entrypoint.
//! Anything left over is reported as unaccounted, which is the finding.

use serde::{Deserialize, Serialize};

use super::graph::{Graph, NodeKind};
use super::reachability::{Reachability, ReachabilityIndex};
use super::{
    is_config_path, is_documentation_path, is_generated_path, is_test_path, EntryPointKind,
    Exposure,
};
use crate::index::LineRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Classification {
    /// Serves an externally reachable capability.
    Feature,
    /// Runs at startup or wires the app together.
    Infrastructure,
    /// A helper reached from real code.
    Utility,
    /// Test code — verification evidence, not shipped behaviour.
    Test,
    /// Configuration and manifests.
    Configuration,
    /// Documentation: promises, not implementations.
    Documentation,
    /// Generated or vendored.
    Generated,
    /// Nothing references it and no root reaches it.
    Dead,
    /// Cannot be classified from static evidence.
    Unknown,
}

impl Classification {
    pub fn label(self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::Infrastructure => "infrastructure",
            Self::Utility => "utility",
            Self::Test => "test",
            Self::Configuration => "configuration",
            Self::Documentation => "documentation",
            Self::Generated => "generated",
            Self::Dead => "dead",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this classification explains the node's existence on its own.
    ///
    /// `Dead` and `Unknown` deliberately do not: dead code is a finding, and
    /// unclassifiable code is exactly what the audit exists to surface.
    pub fn is_accounted(self) -> bool {
        matches!(
            self,
            Self::Feature
                | Self::Infrastructure
                | Self::Utility
                | Self::Test
                | Self::Configuration
                | Self::Documentation
                | Self::Generated
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageFinding {
    pub node_id: String,
    pub name: String,
    pub file: String,
    pub line_range: LineRange,
    pub classification: Classification,
    pub reachability: Reachability,
    pub reason: String,
    /// True when this is a named type rather than a function. An unused model
    /// or interface reads differently to an unused helper, so the report keeps
    /// them apart.
    #[serde(default)]
    pub is_type: bool,
    /// For a symbol whose name occurs elsewhere although nothing traced
    /// reaches it: the first such occurrence, as `path:line`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentioned_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassificationCounts {
    pub feature: usize,
    pub infrastructure: usize,
    pub utility: usize,
    pub test: usize,
    pub configuration: usize,
    pub documentation: usize,
    pub generated: usize,
    pub dead: usize,
    pub unknown: usize,
}

impl ClassificationCounts {
    fn add(&mut self, classification: Classification) {
        match classification {
            Classification::Feature => self.feature += 1,
            Classification::Infrastructure => self.infrastructure += 1,
            Classification::Utility => self.utility += 1,
            Classification::Test => self.test += 1,
            Classification::Configuration => self.configuration += 1,
            Classification::Documentation => self.documentation += 1,
            Classification::Generated => self.generated += 1,
            Classification::Dead => self.dead += 1,
            Classification::Unknown => self.unknown += 1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub meaningful_nodes: usize,
    pub accounted_nodes: usize,
    pub unaccounted_nodes: usize,
    pub coverage_percent: f64,
    pub entrypoint_count: usize,
    pub external_entrypoint_count: usize,
    pub counts: ClassificationCounts,
    /// Nodes reached only along an inferred polymorphic call. Counted as
    /// covered, but the report says so rather than claiming proof.
    #[serde(default)]
    pub dynamic_only_nodes: usize,
    /// Symbols a consumer outside the repository can call directly. For a
    /// library this is the equivalent of an entrypoint count.
    #[serde(default)]
    pub public_api_nodes: usize,
    /// Public symbols nothing inside the repository uses. Expected for a
    /// published library; dead code in an application.
    #[serde(default)]
    pub public_unused: Vec<CoverageFinding>,
    /// Symbols that would have been reported dead, but whose name occurs
    /// somewhere outside their definition. Counted under `unknown`; listed in
    /// `unaccounted` with reachability `mentioned_but_not_traced`.
    #[serde(default)]
    pub mentioned_not_traced: usize,
    /// Only the nodes nobody can account for. This is the actionable list.
    pub unaccounted: Vec<CoverageFinding>,
}

/// Classify every meaningful node and report what cannot be accounted for.
pub fn review(graph: &Graph, reachability: &ReachabilityIndex) -> CoverageReport {
    let mut counts = ClassificationCounts::default();
    let mut unaccounted = Vec::new();
    let mut meaningful = 0usize;
    let mut accounted = 0usize;
    let mut dynamic_only = 0usize;
    let mut public_api = 0usize;
    let mut public_unused: Vec<CoverageFinding> = Vec::new();
    let mut mentioned_not_traced = 0usize;
    let mention_sites: std::collections::HashMap<&str, Option<&str>> = graph
        .untraced_mentions
        .iter()
        .map(|mention| (mention.node_id.as_str(), mention.first_mention.as_deref()))
        .collect();

    for node in &graph.nodes {
        if !is_meaningful(node) {
            continue;
        }
        meaningful += 1;

        let status = reachability.status(&node.id);
        let classification = classify(node, status);
        counts.add(classification);
        if status == Reachability::ReachableDynamic {
            dynamic_only += 1;
        }
        if status == Reachability::MentionedNotTraced {
            mentioned_not_traced += 1;
        }
        if node.is_public && node.kind == NodeKind::Function {
            public_api += 1;
        }
        if status == Reachability::PublicUnused {
            public_unused.push(CoverageFinding {
                node_id: node.id.clone(),
                name: node.name.clone(),
                file: node.file.clone(),
                line_range: node.line_range,
                classification,
                reachability: status,
                reason: "Exported, but nothing inside this repository calls it. Expected if consumers use it; dead code if not.".to_owned(),
                is_type: node.symbol_kind == Some(crate::index::SymbolKind::Class),
                mentioned_at: None,
            });
        }

        if classification.is_accounted() {
            accounted += 1;
            continue;
        }

        unaccounted.push(CoverageFinding {
            node_id: node.id.clone(),
            name: node.name.clone(),
            file: node.file.clone(),
            line_range: node.line_range,
            classification,
            reachability: status,
            reason: reason_for(classification, status),
            is_type: node.symbol_kind == Some(crate::index::SymbolKind::Class),
            mentioned_at: if status == Reachability::MentionedNotTraced {
                mention_sites
                    .get(node.id.as_str())
                    .copied()
                    .flatten()
                    .map(str::to_owned)
            } else {
                None
            },
        });
    }

    unaccounted.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line_range.start.cmp(&right.line_range.start))
            .then(left.name.cmp(&right.name))
    });

    let entrypoint_count = graph.entrypoints().count();
    let external_entrypoint_count = graph
        .entrypoints()
        .filter(|node| node.exposure == Some(Exposure::External))
        .count();

    CoverageReport {
        meaningful_nodes: meaningful,
        accounted_nodes: accounted,
        unaccounted_nodes: unaccounted.len(),
        coverage_percent: if meaningful == 0 {
            100.0
        } else {
            round2((accounted as f64 / meaningful as f64) * 100.0)
        },
        entrypoint_count,
        external_entrypoint_count,
        counts,
        dynamic_only_nodes: dynamic_only,
        public_api_nodes: public_api,
        public_unused,
        mentioned_not_traced,
        unaccounted,
    }
}

/// File nodes are containers, not claims. Functions and entrypoints are what a
/// human has to be able to explain.
fn is_meaningful(node: &super::graph::GraphNode) -> bool {
    match node.kind {
        NodeKind::File => false,
        NodeKind::Function | NodeKind::EntryPoint => true,
    }
}

fn classify(node: &super::graph::GraphNode, status: Reachability) -> Classification {
    // Path-based classifications win: a helper inside a test file is test code
    // no matter what reaches it.
    if is_generated_path(&node.file) {
        return Classification::Generated;
    }
    if is_test_path(&node.file) || node.in_test_scope {
        return Classification::Test;
    }
    if is_documentation_path(&node.file) {
        return Classification::Documentation;
    }
    if is_config_path(&node.file) {
        return Classification::Configuration;
    }

    if node.kind == NodeKind::EntryPoint {
        return match node.exposure {
            Some(Exposure::Startup) => Classification::Infrastructure,
            _ => match node.entrypoint_kind {
                Some(EntryPointKind::Startup) => Classification::Infrastructure,
                _ => Classification::Feature,
            },
        };
    }

    match status {
        // Reached through polymorphic dispatch still means reached. The path is
        // inferred rather than proven, which the verdict reports separately.
        Reachability::Reachable | Reachability::ReachableDynamic => Classification::Feature,
        // Public API nothing here uses is accounted for — it exists to be
        // called from outside — but the report names it, because for an
        // application it is dead code with a `pub` in front.
        Reachability::PublicUnused => Classification::Feature,
        Reachability::InternallyReachable => Classification::Infrastructure,
        Reachability::TestOnly => Classification::Utility,
        Reachability::DeadOrUnreferenced => Classification::Dead,
        Reachability::MentionedNotTraced | Reachability::Unknown => Classification::Unknown,
    }
}


fn reason_for(classification: Classification, status: Reachability) -> String {
    match classification {
        Classification::Dead => {
            "Nothing references this and no entrypoint reaches it. Either it is dead code or it is wired up in a way static analysis cannot see."
                .to_owned()
        }
        Classification::Unknown if status == Reachability::MentionedNotTraced => {
            "No traced call or reference reaches this, but its name appears elsewhere in the repository (see mentioned_at). That may be a use no extractor follows (a macro, a build file, a config, a string, another language), so it is not reported as dead."
                .to_owned()
        }
        Classification::Unknown => format!(
            "Something references this, but no entrypoint, startup path, or test reaches it (reachability: {}). It cannot be tied to anything the software does.",
            status.label()
        ),
        other => format!("Classified as {}.", other.label()),
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::graph::{Edge, EdgeType, GraphNode};

    fn node(id: &str, file: &str, kind: NodeKind) -> GraphNode {
        GraphNode {
            id: id.to_owned(),
            kind,
            name: id.to_owned(),
            file: file.to_owned(),
            line_range: LineRange { start: 1, end: 5 },
            language: "javascript".to_owned(),
            entrypoint_kind: if kind == NodeKind::EntryPoint {
                Some(EntryPointKind::Http)
            } else {
                None
            },
            exposure: if kind == NodeKind::EntryPoint {
                Some(Exposure::External)
            } else {
                None
            },
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        }
    }

    #[test]
    fn reachable_code_is_a_feature_and_counts_as_covered() {
        let graph = Graph {
            nodes: vec![
                node("ep", "server.js", NodeKind::EntryPoint),
                node("handler", "server.js", NodeKind::Function),
            ],
            edges: vec![Edge {
                source: "ep".to_owned(),
                target: "handler".to_owned(),
                edge_type: EdgeType::HandledBy,
            }],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let report = review(&graph, &reachability);

        assert_eq!(report.meaningful_nodes, 2);
        assert_eq!(report.unaccounted_nodes, 0);
        assert_eq!(report.coverage_percent, 100.0);
    }

    #[test]
    fn orphan_code_is_reported_as_unaccounted() {
        let graph = Graph {
            nodes: vec![node("orphan", "src/thing.js", NodeKind::Function)],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let report = review(&graph, &reachability);

        assert_eq!(report.unaccounted_nodes, 1);
        assert_eq!(report.coverage_percent, 0.0);
        assert_eq!(
            report.unaccounted[0].classification,
            Classification::Dead
        );
    }

    #[test]
    fn test_helpers_are_accounted_for_without_being_features() {
        let graph = Graph {
            nodes: vec![node("helper", "src/login.test.js", NodeKind::Function)],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let report = review(&graph, &reachability);

        assert_eq!(report.unaccounted_nodes, 0);
        assert_eq!(report.counts.test, 1);
        assert_eq!(report.counts.feature, 0);
    }

    #[test]
    fn generated_code_never_counts_against_the_repository() {
        let graph = Graph {
            nodes: vec![node("bundled", "dist/bundle.js", NodeKind::Function)],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let report = review(&graph, &reachability);

        assert_eq!(report.unaccounted_nodes, 0);
        assert_eq!(report.counts.generated, 1);
    }

    #[test]
    fn empty_repository_is_fully_covered() {
        let graph = Graph::default();
        let reachability = ReachabilityIndex::build(&graph);
        let report = review(&graph, &reachability);

        assert_eq!(report.coverage_percent, 100.0);
        assert_eq!(report.meaningful_nodes, 0);
    }
}
