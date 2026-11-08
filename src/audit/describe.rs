//! Plain-English descriptions, and documentation reconciliation.
//!
//! This is the only place a model is involved in an audit, and it is
//! deliberately narrow. The graph has already established what exists and what
//! is reachable; the model is not asked to discover anything, only to say in
//! English what the facts already show. That is why it costs cents rather than
//! dollars: the expensive way to do this is to let a model explore the
//! repository one tool call at a time, and that work is already done,
//! deterministically, before the model is asked anything.
//!
//! Two stages, one agent:
//!
//! 1. **Describe** — one call per feature, seeing only that feature's proven
//!    facts. It cannot invent an endpoint because it never sees a blank page.
//! 2. **Reconcile** — one call, seeing the README alongside every description,
//!    to find promises the code does not keep. This has to be global: a
//!    per-feature call cannot tell whether some *other* feature fulfils a claim.
//!
//! Every node id the model cites is checked against the graph. Prose cannot be
//! verified; citations can, so unverifiable citations are dropped rather than
//! reported.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::claims::ClaimSet;
use super::graph::{EdgeType, Graph, NodeKind};
use super::reachability::ReachabilityIndex;
use super::{AuditError, AuditReport, Exposure};
use crate::cache::CacheStore;
use crate::llm::{LlmProvider, RenderedPrompt};

/// Cap on features described in one run, so a huge monorepo cannot run up an
/// unbounded bill without the operator choosing to.
const MAX_FEATURES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureDescription {
    /// The file the feature's entrypoints live in.
    pub area: String,
    /// What this part of the system does, in plain English.
    pub summary: String,
    /// Entrypoint names belonging to this feature.
    pub entrypoints: Vec<String>,
    /// Whether any test reaches any of these entrypoints.
    pub tested: bool,
    /// Graph node ids the model cited, after verification.
    pub cited_nodes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnfulfilledPromise {
    /// The claim, quoted or closely paraphrased from where it was made.
    pub promise: String,
    /// Why the code does not appear to keep it.
    pub reason: String,
    /// Where the claim was made, so a reader can go and check.
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrative {
    pub features: Vec<FeatureDescription>,
    /// Documented claims with no matching implementation.
    pub unfulfilled_promises: Vec<UnfulfilledPromise>,
    pub model: String,
    pub requests: usize,
    pub cached_requests: usize,
}

/// A group of entrypoints that belong together, with the facts to describe them.
#[derive(Debug, Clone)]
struct FeatureCluster {
    area: String,
    entrypoints: Vec<EntrypointFact>,
    tested: bool,
}

#[derive(Debug, Clone)]
struct EntrypointFact {
    name: String,
    node_id: String,
    handler: Option<String>,
    handler_node_id: Option<String>,
    /// What the handler goes on to call, one level deep.
    calls: Vec<String>,
    tested: bool,
}

/// Describe a repository's features, then reconcile its documentation.
pub fn narrate(
    report: &AuditReport,
    graph: &Graph,
    reachability: &ReachabilityIndex,
    provider: &mut dyn LlmProvider,
    cache: Option<&CacheStore>,
) -> Result<Narrative, AuditError> {
    let clusters = cluster(graph, reachability);
    let node_ids: HashSet<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();

    let mut features = Vec::new();
    let mut requests = 0usize;
    let mut cached = 0usize;

    for cluster in clusters.iter().take(MAX_FEATURES) {
        let prompt = describe_prompt(cluster);
        let (raw, was_cached) = complete_cached(provider, cache, &prompt)?;
        requests += 1;
        if was_cached {
            cached += 1;
        }

        let parsed = parse_description(&raw);
        let Some((summary, cited)) = parsed else {
            continue;
        };

        features.push(FeatureDescription {
            area: cluster.area.clone(),
            summary,
            entrypoints: cluster
                .entrypoints
                .iter()
                .map(|entrypoint| entrypoint.name.clone())
                .collect(),
            tested: cluster.tested,
            // Citations the graph cannot confirm are dropped, not reported.
            cited_nodes: cited
                .into_iter()
                .filter(|id| node_ids.contains(id.as_str()))
                .collect(),
        });
    }

    let mut unfulfilled_promises = Vec::new();
    if !features.is_empty() && !report.claims.is_empty() {
        let prompt = reconcile_prompt(&report.claims, &features, report, graph);
        let (raw, was_cached) = complete_cached(provider, cache, &prompt)?;
        requests += 1;
        if was_cached {
            cached += 1;
        }
        unfulfilled_promises = parse_promises(&raw);
    }

    Ok(Narrative {
        features,
        unfulfilled_promises,
        model: provider.model().to_owned(),
        requests,
        cached_requests: cached,
    })
}

/// What each named function calls, by caller name.
fn outgoing_calls(graph: &Graph) -> BTreeMap<&str, Vec<&str>> {
    let by_id: BTreeMap<&str, &str> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.name.as_str()))
        .collect();

    let mut map: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &graph.edges {
        if !matches!(edge.edge_type, EdgeType::Calls | EdgeType::DynamicCalls) {
            continue;
        }
        let (Some(source), Some(target)) = (
            by_id.get(edge.source.as_str()),
            by_id.get(edge.target.as_str()),
        ) else {
            continue;
        };
        let entry = map.entry(*source).or_default();
        if entry.len() < 10 && !entry.contains(target) {
            entry.push(target);
        }
    }
    map
}

/// Ask the provider, reusing a cached answer for an identical prompt and model.
fn complete_cached(
    provider: &mut dyn LlmProvider,
    cache: Option<&CacheStore>,
    prompt: &RenderedPrompt,
) -> Result<(String, bool), AuditError> {
    let hash = prompt.prompt_hash();
    let model = provider.model().to_owned();

    if let Some(store) = cache {
        if let Some(hit) = store.get_llm_response(&hash, &model)? {
            return Ok((hit, true));
        }
    }

    let response = provider
        .complete(prompt)
        .map_err(|error| AuditError::Model(Box::new(error)))?;

    if let Some(store) = cache {
        store.put_llm_response(&hash, &model, &response)?;
    }
    Ok((response, false))
}

/// Group entrypoints by the file they are declared in.
///
/// Web frameworks already organise by feature this way — one router per file —
/// so the file boundary is the feature boundary without having to guess at one.
fn cluster(graph: &Graph, reachability: &ReachabilityIndex) -> Vec<FeatureCluster> {
    let handlers: BTreeMap<&str, &str> = graph
        .edges
        .iter()
        .filter(|edge| edge.edge_type == EdgeType::HandledBy)
        .map(|edge| (edge.source.as_str(), edge.target.as_str()))
        .collect();

    let node_by_id: BTreeMap<&str, &super::graph::GraphNode> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();

    let mut grouped: BTreeMap<String, Vec<EntrypointFact>> = BTreeMap::new();

    // Test files are verification scaffolding, not product surface. Describing
    // them as features tells a reader the test suite is something the software
    // does.
    for node in graph.nodes.iter().filter(|node| {
        node.kind == NodeKind::EntryPoint
            && node.exposure == Some(Exposure::External)
            && !super::is_test_path(&node.file)
    }) {
        let handler_id = handlers.get(node.id.as_str()).copied();
        let handler = handler_id
            .and_then(|id| node_by_id.get(id))
            .map(|handler| handler.name.clone());

        // One level of outgoing calls is enough to say what a handler does
        // without pulling in the whole transitive closure.
        let calls: Vec<String> = handler_id
            .map(|id| {
                graph
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.source == id
                            && matches!(edge.edge_type, EdgeType::Calls | EdgeType::DynamicCalls)
                    })
                    .filter_map(|edge| node_by_id.get(edge.target.as_str()))
                    .map(|target| target.name.clone())
                    .take(8)
                    .collect()
            })
            .unwrap_or_default();

        let tested = handler_id
            .map(|id| reachability.reached_by_tests(id))
            .unwrap_or(false);

        grouped
            .entry(node.file.clone())
            .or_default()
            .push(EntrypointFact {
                name: node.name.clone(),
                node_id: node.id.clone(),
                handler,
                handler_node_id: handler_id.map(str::to_owned),
                calls,
                tested,
            });
    }

    let mut clusters: Vec<FeatureCluster> = grouped
        .into_iter()
        .map(|(area, entrypoints)| FeatureCluster {
            tested: entrypoints.iter().any(|entrypoint| entrypoint.tested),
            area,
            entrypoints,
        })
        .collect();

    // Biggest surfaces first, so a capped run describes what matters most.
    clusters.sort_by(|left, right| {
        right
            .entrypoints
            .len()
            .cmp(&left.entrypoints.len())
            .then(left.area.cmp(&right.area))
    });
    clusters
}

fn describe_prompt(cluster: &FeatureCluster) -> RenderedPrompt {
    let mut facts = String::new();
    facts.push_str(&format!("File: {}\n\n", cluster.area));

    for entrypoint in &cluster.entrypoints {
        facts.push_str(&format!("- {}\n", entrypoint.name));
        facts.push_str(&format!("  id: {}\n", entrypoint.node_id));
        if let Some(handler) = &entrypoint.handler {
            facts.push_str(&format!("  handled by: {handler}\n"));
        }
        if let Some(id) = &entrypoint.handler_node_id {
            facts.push_str(&format!("  handler id: {id}\n"));
        }
        if !entrypoint.calls.is_empty() {
            facts.push_str(&format!("  which calls: {}\n", entrypoint.calls.join(", ")));
        }
        facts.push_str(&format!(
            "  covered by a test: {}\n",
            if entrypoint.tested { "yes" } else { "no" }
        ));
    }

    RenderedPrompt {
        system: "You describe software for a non-technical reader.\n\
                 You are given verified facts about one part of a system. Every entrypoint, \
                 handler, and call listed was proven to exist by static analysis.\n\n\
                 Rules:\n\
                 - Describe only what the facts show. Never invent an endpoint, behaviour, or \
                   guarantee that is not listed.\n\
                 - Do not claim anything works correctly. You can see that code exists, not \
                   that it is right.\n\
                 - Two or three sentences. Plain words. No jargon, no marketing.\n\
                 - Name what a user can do, not how the code is structured.\n\n\
                 Reply with JSON only: {\"summary\": \"...\", \"cited\": [\"id\", ...]}\n\
                 `cited` lists the ids you actually relied on, copied exactly."
            .to_owned(),
        user: facts,
    }
}

fn reconcile_prompt(
    claim_set: &ClaimSet,
    features: &[FeatureDescription],
    report: &AuditReport,
    graph: &Graph,
) -> RenderedPrompt {
    let mut body = String::from("What the code actually exposes:\n\n");
    for feature in features {
        body.push_str(&format!("- {}: {}\n", feature.area, feature.summary));
    }

    body.push_str("\nEvery entrypoint found:\n");
    for entrypoint in report.entrypoints.iter().take(200) {
        body.push_str(&format!("- {}\n", entrypoint.name));
    }

    // The complete name inventory. Without this the model cannot tell the
    // difference between "the code does not do refunds" and "I was not shown
    // the part that does" — every claim looks plausible against a list of
    // endpoint names alone.
    body.push_str(
        "\nEvery function and type name in the codebase. This list is complete: if a \
         capability is not represented here, no code implements it.\n",
    );
    let mut names: Vec<&str> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Function)
        .map(|node| node.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    for chunk in names.chunks(12) {
        body.push_str(&format!("  {}\n", chunk.join(", ")));
    }

    // For claims attached to a specific function, show what that function
    // actually calls. A docstring promising a refund and an email, on a
    // function that calls nothing, is checkable rather than a guess.
    let call_targets = outgoing_calls(graph);
    let subjects: Vec<&crate::audit::claims::Claim> = claim_set
        .claims
        .iter()
        .filter(|claim| claim.subject.is_some())
        .collect();
    if !subjects.is_empty() {
        body.push_str("\nWhat each documented function actually calls:\n");
        for claim in subjects.iter().take(120) {
            let Some(subject) = &claim.subject else {
                continue;
            };
            let calls = call_targets
                .get(subject.as_str())
                .map(|targets| targets.join(", "))
                .unwrap_or_else(|| "nothing".to_owned());
            body.push_str(&format!("  {subject} calls: {calls}\n"));
        }
    }

    body.push_str(
        "\n\nWhat the project says about itself. Each line is tagged with where the claim \
         came from and the file and line it is on:\n\n",
    );
    body.push_str(&claim_set.render());

    RenderedPrompt {
        system: "You compare what a project claims about itself against what its code actually \
                 contains.\n\n\
                 You are given a verified inventory of everything the code exposes, then every \
                 claim the project makes, each tagged with its source.\n\n\
                 Find claims the inventory does not support — a promised behaviour with no \
                 matching endpoint or function.\n\n\
                 The inventory of function and type names is complete. If a capability has no \
                 corresponding name anywhere in it, and no described feature covers it, then \
                 no code implements it — say so rather than assuming you were shown a partial \
                 view.\n\n\
                 When a claim is a docstring on a function, check it against what that \
                 function actually calls. A docstring promising work the function does not \
                 perform, and does not delegate to anything that could perform it, is unmet.\n\n\
                 Weigh the sources differently:\n\
                 - `docstring` is attached to a specific function and is what an integrator \
                   reads. A docstring describing behaviour the code does not have is the most \
                   serious kind of mismatch.\n\
                 - `api spec` is a contract consumers build against.\n\
                 - `readme`, `documentation`, and `changelog` are looser; only report these \
                   when the gap is clear.\n\n\
                 Rules:\n\
                 - Only report a claim when nothing in the inventory plausibly implements it.\n\
                 - Ignore setup instructions, install steps, licences, and how to run the \
                   project. Those are not claims about what the software does.\n\
                 - If everything claimed is present, return an empty list. Do not invent \
                   problems to seem useful — but do not stay silent about a real gap either. \
                   A claim with no supporting name in a complete inventory is a real gap.\n\
                 - Quote the claim and name the file it came from so a reader can find it.\n\n\
                 Reply with JSON only: \
                 [{\"promise\": \"...\", \"reason\": \"...\", \"source\": \"file:line\"}]"
            .to_owned(),
        user: body,
    }
}

/// Pull `{"summary": ..., "cited": [...]}` out of a model reply.
fn parse_description(raw: &str) -> Option<(String, Vec<String>)> {
    let value: serde_json::Value = serde_json::from_str(&extract_json(raw)?).ok()?;
    let summary = value.get("summary")?.as_str()?.trim().to_owned();
    if summary.is_empty() {
        return None;
    }
    let cited = value
        .get("cited")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some((summary, cited))
}

fn parse_promises(raw: &str) -> Vec<UnfulfilledPromise> {
    let Some(text) = extract_json(raw) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| {
            let promise = item.get("promise")?.as_str()?.trim().to_owned();
            let reason = item
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            let source = item
                .get("source")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            if promise.is_empty() {
                return None;
            }
            Some(UnfulfilledPromise {
                promise,
                reason,
                source,
            })
        })
        .collect()
}

/// Models wrap JSON in prose or fences often enough that this is worth doing.
fn extract_json(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|rest| rest.trim_start())
        .and_then(|rest| rest.strip_suffix("```").or(Some(rest)))
        .unwrap_or(trimmed)
        .trim();

    let start = without_fence.find(['{', '['])?;
    let opening = without_fence.as_bytes()[start];
    let closing = if opening == b'{' { b'}' } else { b']' };
    let end = without_fence.rfind(closing as char)?;
    if end <= start {
        return None;
    }
    Some(without_fence[start..=end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clean_json_reply() {
        let raw = r#"{"summary": "Customers can book a car.", "cited": ["entrypoint:http:a.py:1:POST /book"]}"#;
        let (summary, cited) = parse_description(raw).expect("parsed");
        assert_eq!(summary, "Customers can book a car.");
        assert_eq!(cited, vec!["entrypoint:http:a.py:1:POST /book"]);
    }

    #[test]
    fn parses_json_wrapped_in_a_code_fence() {
        let raw = "```json\n{\"summary\": \"Users log in.\", \"cited\": []}\n```";
        let (summary, _) = parse_description(raw).expect("parsed");
        assert_eq!(summary, "Users log in.");
    }

    #[test]
    fn parses_json_surrounded_by_prose() {
        let raw = "Here you go:\n{\"summary\": \"It sends email.\", \"cited\": []}\nHope that helps.";
        let (summary, _) = parse_description(raw).expect("parsed");
        assert_eq!(summary, "It sends email.");
    }

    #[test]
    fn a_reply_with_no_summary_is_discarded_rather_than_guessed_at() {
        assert!(parse_description("{\"cited\": []}").is_none());
        assert!(parse_description("not json at all").is_none());
        assert!(parse_description("{\"summary\": \"   \"}").is_none());
    }

    use crate::audit::graph::{Edge, GraphNode};
    use crate::audit::{EntryPointKind, Exposure};
    use crate::index::LineRange;
    use crate::llm::LlmError;

    /// A provider that returns a canned reply, so the pipeline around the model
    /// can be tested without one.
    struct FakeProvider {
        reply: String,
        calls: std::cell::Cell<usize>,
    }

    impl FakeProvider {
        fn new(reply: &str) -> Self {
            Self {
                reply: reply.to_owned(),
                calls: std::cell::Cell::new(0),
            }
        }
    }

    impl crate::llm::LlmProvider for FakeProvider {
        fn provider_name(&self) -> &str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-model"
        }
        fn complete(&mut self, _prompt: &RenderedPrompt) -> Result<String, LlmError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.reply.clone())
        }
        fn stream_explanation(
            &mut self,
            _prompt: &RenderedPrompt,
            _sink: &mut dyn FnMut(&str) -> Result<(), LlmError>,
        ) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn function_node(id: &str, name: &str, file: &str) -> GraphNode {
        GraphNode {
            id: id.to_owned(),
            kind: NodeKind::Function,
            name: name.to_owned(),
            file: file.to_owned(),
            line_range: LineRange { start: 1, end: 5 },
            language: "python".to_owned(),
            entrypoint_kind: None,
            exposure: None,
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        }
    }

    fn entrypoint_node(id: &str, name: &str, file: &str) -> GraphNode {
        GraphNode {
            id: id.to_owned(),
            kind: NodeKind::EntryPoint,
            name: name.to_owned(),
            file: file.to_owned(),
            line_range: LineRange { start: 1, end: 1 },
            language: "python".to_owned(),
            entrypoint_kind: Some(EntryPointKind::Http),
            exposure: Some(Exposure::External),
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        }
    }

    #[test]
    fn features_are_grouped_by_file_biggest_first() {
        let graph = Graph {
            nodes: vec![
                entrypoint_node("ep:a1", "GET /a", "routes/small.py"),
                entrypoint_node("ep:b1", "GET /b", "routes/big.py"),
                entrypoint_node("ep:b2", "POST /b", "routes/big.py"),
            ],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let clusters = cluster(&graph, &reachability);

        assert_eq!(clusters.len(), 2);
        // A capped run should describe the largest surfaces first.
        assert_eq!(clusters[0].area, "routes/big.py");
        assert_eq!(clusters[0].entrypoints.len(), 2);
    }

    #[test]
    fn a_test_file_is_never_described_as_a_feature() {
        // Describing a test suite as something the software does tells the
        // reader the wrong thing entirely.
        let graph = Graph {
            nodes: vec![
                entrypoint_node("ep:real", "GET /users", "routes/users.py"),
                entrypoint_node("ep:test", "GET /fixture", "tests/test_users.py"),
            ],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let clusters = cluster(&graph, &reachability);

        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].area, "routes/users.py");
    }

    #[test]
    fn a_cited_node_that_does_not_exist_is_dropped() {
        // Prose cannot be checked, but citations can. A model naming a node the
        // graph does not contain must not have that citation reach the report.
        let graph = Graph {
            nodes: vec![
                entrypoint_node("ep:real", "GET /users", "routes/users.py"),
                function_node("fn:real", "list_users", "routes/users.py"),
            ],
            edges: vec![Edge {
                source: "ep:real".to_owned(),
                target: "fn:real".to_owned(),
                edge_type: EdgeType::HandledBy,
            }],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let mut report = blank_report();
        let mut provider = FakeProvider::new(
            r#"{"summary": "Users can be listed.", "cited": ["ep:real", "ep:invented"]}"#,
        );

        let narrative = narrate(&report, &graph, &reachability, &mut provider, None)
            .expect("narrated");
        report.narrative = Some(narrative.clone());

        assert_eq!(narrative.features.len(), 1);
        let cited = &narrative.features[0].cited_nodes;
        assert!(cited.contains(&"ep:real".to_owned()), "{cited:?}");
        assert!(
            !cited.contains(&"ep:invented".to_owned()),
            "an unverifiable citation must not survive: {cited:?}"
        );
    }

    #[test]
    fn an_unparseable_reply_produces_no_feature_rather_than_a_guess() {
        let graph = Graph {
            nodes: vec![entrypoint_node("ep:real", "GET /users", "routes/users.py")],
            edges: Vec::new(),
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };
        let reachability = ReachabilityIndex::build(&graph);
        let report = blank_report();
        let mut provider = FakeProvider::new("the model rambled instead of answering");

        let narrative =
            narrate(&report, &graph, &reachability, &mut provider, None).expect("narrated");
        assert!(narrative.features.is_empty());
    }

    /// An otherwise-empty report, for exercising the description pass alone.
    fn blank_report() -> AuditReport {
        AuditReport {
            root: std::path::PathBuf::from("."),
            files: 0,
            symbols: 0,
            entrypoints: Vec::new(),
            languages: Default::default(),
            coverage: Default::default(),
            tests: Default::default(),
            score: super::super::score::score(&Default::default()),
            verdict: super::super::score::verdict(
                &Default::default(),
                &super::super::score::score(&Default::default()),
            ),
            narrative: None,
            claims: Default::default(),
            vendored: Vec::new(),
            graph: Graph::default(),
        }
    }

    #[test]
    fn outgoing_calls_are_indexed_by_caller_name() {
        use crate::audit::graph::{Edge, GraphNode};
        use crate::index::LineRange;

        let node = |id: &str, name: &str| GraphNode {
            id: id.to_owned(),
            kind: NodeKind::Function,
            name: name.to_owned(),
            file: "a.py".to_owned(),
            line_range: LineRange { start: 1, end: 2 },
            language: "python".to_owned(),
            entrypoint_kind: None,
            exposure: None,
            in_test_scope: false,
            symbol_kind: None,
            is_public: false,
        };
        let graph = Graph {
            nodes: vec![
                node("f:cancel", "cancel_booking"),
                node("f:refund", "issue_refund"),
            ],
            edges: vec![Edge {
                source: "f:cancel".to_owned(),
                target: "f:refund".to_owned(),
                edge_type: EdgeType::Calls,
            }],
            referenced_names: Vec::new(),
            dispatched: Vec::new(),
            opaque_callers: Vec::new(),
            untraced_mentions: Vec::new(),
        };

        let calls = outgoing_calls(&graph);
        assert_eq!(calls.get("cancel_booking"), Some(&vec!["issue_refund"]));
        // A function that calls nothing is absent, which the prompt renders as
        // "calls: nothing" — the evidence that makes a false docstring visible.
        assert!(calls.get("issue_refund").is_none());
    }

    #[test]
    fn parses_a_promise_with_its_source_location() {
        let raw = r#"[{"promise": "Supports refunds", "reason": "No refund function exists.", "source": "README.md:12"}]"#;
        let promises = parse_promises(raw);
        assert_eq!(promises.len(), 1);
        assert_eq!(promises[0].source, "README.md:12");
    }

    #[test]
    fn parses_unfulfilled_promises() {
        let raw = r#"[{"promise": "Supports refunds", "reason": "No endpoint or function mentions refunds."}]"#;
        let promises = parse_promises(raw);
        assert_eq!(promises.len(), 1);
        assert_eq!(promises[0].promise, "Supports refunds");
    }

    #[test]
    fn an_empty_promise_list_means_nothing_to_report() {
        assert!(parse_promises("[]").is_empty());
        assert!(parse_promises("garbage").is_empty());
    }
}
