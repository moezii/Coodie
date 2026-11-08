use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::SourceLanguage;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    /// A named type: class, struct, enum, trait, or interface.
    ///
    /// These are findable in their own right — "where is the Booking model
    /// defined" is a question about a type, not a function — and an unused one
    /// is as much a finding as an unused function.
    Class,
}

impl SymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "type",
        }
    }
}

/// Does this tree-sitter node kind declare a named type?
///
/// Scope captures cover two different things. `class Booking` declares a type
/// that should be findable. `impl Booking` and `mod tests` only establish a
/// scope for the things inside them — they declare nothing new, and emitting
/// them as definitions would produce a duplicate symbol per `impl` block.
pub fn is_type_definition_kind(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "class_definition"
            | "class"
            | "class_specifier"
            | "struct_item"
            | "struct_specifier"
            | "struct_declaration"
            | "enum_item"
            | "enum_declaration"
            | "enum_specifier"
            | "trait_item"
            | "interface_declaration"
            | "type_declaration"
            | "type_alias_declaration"
            | "type_item"
            | "object_declaration"
            | "record_declaration"
            | "union_item"
            | "type_spec"
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub language: SourceLanguage,
    pub file: PathBuf,
    pub byte_range: ByteRange,
    pub line_range: LineRange,
    pub enclosing_scope: String,
    /// Whether a consumer outside this file can reach the symbol.
    ///
    /// For a library this is the equivalent of an HTTP route: the public API
    /// is how the outside world gets in. Without it, every exported function
    /// in a library reads as unreachable.
    #[serde(default)]
    pub is_public: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_of_a_named_type_are_definitions() {
        for kind in [
            "class_declaration",
            "class_definition",
            "struct_item",
            "enum_declaration",
            "trait_item",
            "interface_declaration",
            "type_declaration",
        ] {
            assert!(
                is_type_definition_kind(kind),
                "{kind} declares a findable type"
            );
        }
    }

    #[test]
    fn scope_only_constructs_are_not_definitions() {
        // `impl Booking` and `mod tests` introduce no new name. Treating them as
        // definitions would emit one duplicate symbol per impl block, and would
        // make a module look like a type.
        for kind in ["impl_item", "mod_item", "declaration_list", "block"] {
            assert!(
                !is_type_definition_kind(kind),
                "{kind} only establishes a scope"
            );
        }
    }

    #[test]
    fn symbol_kinds_are_labelled_for_the_report() {
        assert_eq!(SymbolKind::Function.label(), "function");
        assert_eq!(SymbolKind::Class.label(), "type");
    }
}
