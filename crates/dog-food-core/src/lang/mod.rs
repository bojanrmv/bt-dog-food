mod csharp;
mod prepare;
mod rust;
mod typescript;

use std::path::Path;

use serde::Serialize;
use tree_sitter::Node;

/// Rules for one language. Analyzers only say *what* counts; the walker owns
/// *how* counting works.
pub(crate) trait LanguageAnalyzer: Sync {
    fn grammar(&self) -> tree_sitter::Language;
    /// The source rewritten, at the same length, into syntax the grammar
    /// knows, mildest first; empty when the language has nothing to rewrite.
    fn prepare(&self, _source: &str) -> Vec<String> {
        Vec::new()
    }
    /// `Some(kind)` if this node starts a new function scope.
    fn function_kind(&self, node: Node) -> Option<&'static str>;
    /// `Some(category)` if this node is a decision point (adds 1).
    fn decision(&self, node: Node, src: &[u8]) -> Option<&'static str>;
    /// How much a decision node adds; 1 unless one node holds several
    /// decisions (a C# switch section with several `case` labels).
    fn weight(&self, _node: Node) -> u32 {
        1
    }
    /// Name of a function node, without its enclosing functions.
    fn name(&self, node: Node, src: &[u8]) -> String;
    /// Whether this node is a branch or loop that nests (cognitive complexity).
    fn nests(&self, _node: Node) -> bool {
        false
    }
}

/// `true` for kinds in `nesting`, except the `if` of an `else if`: its `else`
/// keyword is counted on its own and the chain does not nest deeper.
/// Only nesting kinds reach the parent lookup, which walks the tree and is
/// too slow to do for every node.
fn nests(node: Node, nesting: &[&str]) -> bool {
    nesting.contains(&node.kind()) && !is_else_if(node)
}

fn is_else_if(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "else_clause"
        || parent
            .child_by_field_name("alternative")
            .is_some_and(|alternative| alternative.id() == node.id())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    CSharp,
    Rust,
    TypeScript,
    Tsx,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Language> {
        let ext = path.extension()?.to_str()?;
        match ext {
            "cs" => Some(Language::CSharp),
            "rs" => Some(Language::Rust),
            "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => Some(Language::TypeScript),
            "tsx" | "jsx" => Some(Language::Tsx),
            _ => None,
        }
    }

    pub(crate) fn analyzer(self) -> &'static dyn LanguageAnalyzer {
        match self {
            Language::CSharp => &csharp::CSharpAnalyzer,
            Language::Rust => &rust::RustAnalyzer,
            Language::TypeScript => &typescript::TYPESCRIPT,
            Language::Tsx => &typescript::TSX,
        }
    }
}

/// Text of a node's named field, if present and valid UTF-8.
fn field_text<'a>(node: Node, field: &str, src: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name(field)?.utf8_text(src).ok()
}

/// The category for `kind` in a table of (node kind, category) pairs.
fn category(table: &[(&str, &'static str)], kind: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(candidate, _)| *candidate == kind)
        .map(|(_, category)| *category)
}

/// The operator of a binary expression if it is one of `ops`.
fn logical_operator(node: Node, ops: &[&'static str]) -> Option<&'static str> {
    let op = node.child_by_field_name("operator")?;
    ops.iter()
        .copied()
        .find(|candidate| *candidate == op.kind())
}

/// `line:column` (both 1-based), to tell apart anonymous functions on one line.
fn position(node: Node) -> String {
    let start = node.start_position();
    format!("{}:{}", start.row + 1, start.column + 1)
}
