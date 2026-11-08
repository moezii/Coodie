//! Claim extraction.
//!
//! A promise about what software does is rarely confined to the README. It sits
//! in the docstring above an endpoint, in a `docs/` folder, in an OpenAPI spec,
//! in a package description, in a changelog entry saying a feature shipped.
//!
//! Each source carries different weight. A docstring sits inches from the code
//! and is what a client integrating against an API actually reads, so a
//! docstring that lies is worse than a stale line in a marketing README. The
//! extractor records where each claim came from so the reconciliation can say
//! so.
//!
//! Nothing here calls a model. This is reading and locating text; judging it
//! against the code happens later.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::Node;

use crate::index::SourceLanguage;

/// Largest single document read, in characters.
const MAX_DOC_CHARS: usize = 20_000;
/// Total claim text handed to the model.
const MAX_TOTAL_CHARS: usize = 40_000;
/// Docstrings shorter than this are labels, not promises.
const MIN_DOCSTRING_CHARS: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ClaimSource {
    /// A docstring or comment attached to an entrypoint. Closest to the code,
    /// and what an API consumer reads — the highest-stakes place to be wrong.
    Docstring,
    /// A machine-readable API description.
    ApiSpec,
    /// Prose documentation under `docs/`.
    Documentation,
    /// The project's front-page README.
    Readme,
    /// A package manifest description field.
    Manifest,
    /// A changelog entry announcing a feature.
    Changelog,
}

impl ClaimSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Docstring => "docstring",
            Self::ApiSpec => "api spec",
            Self::Documentation => "documentation",
            Self::Readme => "readme",
            Self::Manifest => "manifest",
            Self::Changelog => "changelog",
        }
    }

    /// Ordering used when the budget forces a choice. Docstrings first: they
    /// are attached to a specific endpoint, so a mismatch is checkable and
    /// consequential rather than a vague marketing overstatement.
    fn priority(self) -> u8 {
        match self {
            Self::Docstring => 0,
            Self::ApiSpec => 1,
            Self::Documentation => 2,
            Self::Readme => 3,
            Self::Manifest => 4,
            Self::Changelog => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub source: ClaimSource,
    pub file: String,
    pub line: usize,
    /// The symbol this claim is attached to, when it is attached to one.
    pub subject: Option<String>,
    pub text: String,
}

/// Everything the repository asserts about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSet {
    pub claims: Vec<Claim>,
}

impl ClaimSet {
    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }

    pub fn len(&self) -> usize {
        self.claims.len()
    }

    /// How many claims came from each source.
    pub fn counts(&self) -> Vec<(ClaimSource, usize)> {
        let mut counts: Vec<(ClaimSource, usize)> = Vec::new();
        for claim in &self.claims {
            match counts.iter_mut().find(|(source, _)| *source == claim.source) {
                Some((_, count)) => *count += 1,
                None => counts.push((claim.source, 1)),
            }
        }
        counts.sort_by_key(|(source, _)| source.priority());
        counts
    }

    /// Render for a prompt, highest-stakes sources first, within a budget.
    pub fn render(&self) -> String {
        let mut ordered = self.claims.clone();
        ordered.sort_by(|left, right| {
            left.source
                .priority()
                .cmp(&right.source.priority())
                .then(left.file.cmp(&right.file))
                .then(left.line.cmp(&right.line))
        });

        let mut out = String::new();
        for claim in ordered {
            let entry = match &claim.subject {
                Some(subject) => format!(
                    "[{}] {}:{} ({}): {}\n",
                    claim.source.label(),
                    claim.file,
                    claim.line,
                    subject,
                    claim.text
                ),
                None => format!(
                    "[{}] {}:{}: {}\n",
                    claim.source.label(),
                    claim.file,
                    claim.line,
                    claim.text
                ),
            };
            if out.len() + entry.len() > MAX_TOTAL_CHARS {
                out.push_str("\n(further claims omitted for length)\n");
                break;
            }
            out.push_str(&entry);
        }
        out
    }
}

/// Collect prose claims from files across the repository.
///
/// Docstrings are gathered separately during parsing, since they need the tree.
pub fn collect_file_claims(root: &Path, relative_paths: &[String]) -> ClaimSet {
    let mut claims = Vec::new();
    let mut seen = BTreeSet::new();

    for relative in relative_paths {
        let Some(source) = classify_document(relative) else {
            continue;
        };
        if !seen.insert(relative.clone()) {
            continue;
        }
        let Ok(text) = fs::read_to_string(root.join(relative)) else {
            continue;
        };
        let trimmed: String = text.chars().take(MAX_DOC_CHARS).collect();
        if trimmed.trim().is_empty() {
            continue;
        }
        claims.push(Claim {
            source,
            file: relative.clone(),
            line: 1,
            subject: None,
            text: trimmed,
        });
    }

    ClaimSet { claims }
}

/// Which kind of claim document is this, if any?
fn classify_document(relative: &str) -> Option<ClaimSource> {
    let lower = relative.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);

    // Generated output and dependencies make claims about other people's code.
    if super::is_generated_path(&lower) || lower.contains("node_modules/") {
        return None;
    }

    if name.starts_with("changelog") {
        return Some(ClaimSource::Changelog);
    }
    if name.starts_with("readme") {
        return Some(ClaimSource::Readme);
    }
    if name == "openapi.json"
        || name == "openapi.yaml"
        || name == "openapi.yml"
        || name == "swagger.json"
        || name == "swagger.yaml"
        || name.ends_with(".openapi.json")
    {
        return Some(ClaimSource::ApiSpec);
    }
    if name == "package.json" || name == "pyproject.toml" || name == "cargo.toml" {
        return Some(ClaimSource::Manifest);
    }
    if (lower.starts_with("docs/") || lower.contains("/docs/"))
        && (lower.ends_with(".md") || lower.ends_with(".mdx") || lower.ends_with(".rst"))
    {
        return Some(ClaimSource::Documentation);
    }

    None
}

/// Pull docstrings attached to named definitions out of a parsed tree.
///
/// Handles Python docstrings (a leading string in the body) and the JavaScript,
/// Java, C#, Go, and Rust convention of a comment block immediately above a
/// definition.
pub fn collect_docstrings(
    language: &SourceLanguage,
    relative: &str,
    source: &[u8],
    root: Node<'_>,
) -> Vec<Claim> {
    let mut claims = Vec::new();
    let mut stack = vec![root];

    while let Some(current) = stack.pop() {
        if is_definition_node(current.kind()) {
            if let Some(name) = current
                .child_by_field_name("name")
                .and_then(|node| node_text(node, source))
            {
                if let Some(text) = docstring_for(current, source, language) {
                    if text.chars().count() >= MIN_DOCSTRING_CHARS {
                        claims.push(Claim {
                            source: ClaimSource::Docstring,
                            file: relative.to_owned(),
                            line: current.start_position().row + 1,
                            subject: Some(name),
                            text,
                        });
                    }
                }
            }
        }

        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    claims.sort_by_key(|claim| claim.line);
    claims
}

fn is_definition_node(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "function_declaration"
            | "function_item"
            | "method_definition"
            | "method_declaration"
            | "class_definition"
            | "class_declaration"
    )
}

/// Python puts its docstring inside the body; most other languages put a
/// comment block immediately above the definition.
fn docstring_for(node: Node<'_>, source: &[u8], language: &SourceLanguage) -> Option<String> {
    if language.id() == "python" {
        if let Some(body) = node.child_by_field_name("body") {
            if let Some(first) = body.named_child(0) {
                if first.kind() == "expression_statement" {
                    if let Some(inner) = first.named_child(0) {
                        if inner.kind().contains("string") {
                            return node_text(inner, source).map(|text| clean_docstring(&text));
                        }
                    }
                }
            }
        }
        return None;
    }

    // Walk backwards over adjacent comment siblings.
    let mut lines: Vec<String> = Vec::new();
    let mut sibling = node.prev_sibling();
    // Decorators and attributes sit between a comment and its definition.
    while let Some(candidate) = sibling {
        let kind = candidate.kind();
        if kind == "decorator" || kind == "attribute_item" || kind == "modifiers" {
            sibling = candidate.prev_sibling();
            continue;
        }
        if !kind.contains("comment") {
            break;
        }
        if let Some(text) = node_text(candidate, source) {
            lines.push(clean_comment(&text));
        }
        sibling = candidate.prev_sibling();
    }

    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    let joined = lines.join(" ").trim().to_owned();
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn clean_docstring(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_quotes = trimmed
        .trim_start_matches("r\"\"\"")
        .trim_start_matches("\"\"\"")
        .trim_start_matches("'''")
        .trim_end_matches("\"\"\"")
        .trim_end_matches("'''")
        .trim_matches('"')
        .trim_matches('\'');
    collapse_whitespace(without_quotes)
}

fn clean_comment(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_markers = trimmed
        .trim_start_matches("/**")
        .trim_start_matches("///")
        .trim_start_matches("//!")
        .trim_start_matches("//")
        .trim_start_matches("/*")
        .trim_start_matches('#')
        .trim_end_matches("*/");
    collapse_whitespace(&without_markers.replace('*', " "))
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn node_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    source
        .get(node.byte_range())
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse(language_id: &str, source: &[u8]) -> (SourceLanguage, tree_sitter::Tree) {
        let language = SourceLanguage::new(language_id).expect("language");
        let mut parser = Parser::new();
        parser
            .set_language(&language.tree_sitter_language().expect("grammar"))
            .expect("set language");
        let tree = parser.parse(source, None).expect("parse");
        (language, tree)
    }

    #[test]
    fn extracts_a_python_docstring_as_a_claim() {
        let source = br#"
def refund_booking(booking_id):
    """Refunds a booking and returns the money to the customer's card."""
    return None
"#;
        let (language, tree) = parse("python", source);
        let claims = collect_docstrings(&language, "api.py", source, tree.root_node());

        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].subject.as_deref(), Some("refund_booking"));
        assert!(claims[0].text.contains("Refunds a booking"));
        assert_eq!(claims[0].source, ClaimSource::Docstring);
    }

    #[test]
    fn a_one_word_docstring_is_a_label_not_a_promise() {
        let source = br#"
def get(id):
    """Get."""
    return None
"#;
        let (language, tree) = parse("python", source);
        assert!(collect_docstrings(&language, "a.py", source, tree.root_node()).is_empty());
    }

    #[test]
    fn extracts_a_javascript_comment_block_above_a_function() {
        let source = br#"
// Sends a confirmation email to the customer after checkout completes.
function notifyCustomer(order) {
  return true;
}
"#;
        let (language, tree) = parse("javascript", source);
        let claims = collect_docstrings(&language, "mail.js", source, tree.root_node());

        assert_eq!(claims.len(), 1, "got {claims:?}");
        assert_eq!(claims[0].subject.as_deref(), Some("notifyCustomer"));
        assert!(claims[0].text.contains("confirmation email"));
    }

    #[test]
    fn collects_claim_documents_and_leaves_source_alone() {
        use std::fs;
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        fs::create_dir_all(root.join("docs")).expect("dirs");
        fs::write(root.join("README.md"), "# App\n\nIt books cars.\n").expect("write");
        fs::write(root.join("docs/booking.md"), "Bookings can be cancelled.\n").expect("write");
        fs::write(root.join("app.py"), "def book():\n    pass\n").expect("write");

        let paths = vec![
            "README.md".to_owned(),
            "docs/booking.md".to_owned(),
            "app.py".to_owned(),
        ];
        let set = collect_file_claims(root, &paths);

        assert_eq!(set.len(), 2, "source files are not claim documents: {set:?}");
        assert!(set
            .claims
            .iter()
            .any(|claim| claim.source == ClaimSource::Readme));
        assert!(set
            .claims
            .iter()
            .any(|claim| claim.source == ClaimSource::Documentation));
    }

    #[test]
    fn a_missing_or_empty_document_contributes_nothing() {
        use std::fs;
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        fs::write(root.join("README.md"), "   \n\n").expect("write");

        let paths = vec!["README.md".to_owned(), "CHANGELOG.md".to_owned()];
        // One is blank, the other does not exist. Neither is a claim.
        assert!(collect_file_claims(root, &paths).is_empty());
    }

    #[test]
    fn the_same_file_listed_twice_is_read_once() {
        use std::fs;
        let directory = tempfile::tempdir().expect("temp dir");
        fs::write(directory.path().join("README.md"), "It books cars.\n").expect("write");

        let paths = vec!["README.md".to_owned(), "README.md".to_owned()];
        assert_eq!(collect_file_claims(directory.path(), &paths).len(), 1);
    }

    #[test]
    fn classifies_documents_by_where_they_live() {
        assert_eq!(classify_document("README.md"), Some(ClaimSource::Readme));
        assert_eq!(
            classify_document("docs/features/booking.md"),
            Some(ClaimSource::Documentation)
        );
        assert_eq!(classify_document("openapi.json"), Some(ClaimSource::ApiSpec));
        assert_eq!(
            classify_document("CHANGELOG.md"),
            Some(ClaimSource::Changelog)
        );
        assert_eq!(
            classify_document("package.json"),
            Some(ClaimSource::Manifest)
        );
        // Source files and generated output are not claim documents.
        assert_eq!(classify_document("src/main.py"), None);
        assert_eq!(classify_document("dist/README.md"), None);
    }

    #[test]
    fn rendering_puts_docstrings_before_marketing_prose() {
        let set = ClaimSet {
            claims: vec![
                Claim {
                    source: ClaimSource::Readme,
                    file: "README.md".to_owned(),
                    line: 1,
                    subject: None,
                    text: "We do everything".to_owned(),
                },
                Claim {
                    source: ClaimSource::Docstring,
                    file: "api.py".to_owned(),
                    line: 10,
                    subject: Some("refund".to_owned()),
                    text: "Refunds a payment".to_owned(),
                },
            ],
        };
        let rendered = set.render();
        let docstring_at = rendered.find("Refunds a payment").expect("docstring");
        let readme_at = rendered.find("We do everything").expect("readme");
        assert!(
            docstring_at < readme_at,
            "docstrings are attached to specific code and rank first"
        );
    }

    #[test]
    fn counts_are_grouped_by_source() {
        let set = ClaimSet {
            claims: vec![
                Claim {
                    source: ClaimSource::Docstring,
                    file: "a.py".to_owned(),
                    line: 1,
                    subject: None,
                    text: "one".to_owned(),
                },
                Claim {
                    source: ClaimSource::Docstring,
                    file: "b.py".to_owned(),
                    line: 1,
                    subject: None,
                    text: "two".to_owned(),
                },
                Claim {
                    source: ClaimSource::Readme,
                    file: "README.md".to_owned(),
                    line: 1,
                    subject: None,
                    text: "three".to_owned(),
                },
            ],
        };
        assert_eq!(
            set.counts(),
            vec![(ClaimSource::Docstring, 2), (ClaimSource::Readme, 1)]
        );
    }
}
