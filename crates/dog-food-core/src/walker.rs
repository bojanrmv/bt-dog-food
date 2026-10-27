use tree_sitter::{Node, Tree};

use crate::lang::LanguageAnalyzer;
use crate::model::FunctionMetrics;

/// A function whose closing node we have not reached yet.
struct Scope {
    node_id: usize,
    metrics: FunctionMetrics,
    /// Ids of the open branch and loop nodes inside this function.
    nesting: Vec<usize>,
}

/// Walks the tree once, depth first. Entering a function node opens a scope,
/// every decision node adds to the innermost open scope, and leaving the
/// function node closes its scope. Decisions outside any function are ignored.
pub(crate) fn walk(
    tree: &Tree,
    src: &[u8],
    analyzer: &dyn LanguageAnalyzer,
) -> Vec<FunctionMetrics> {
    let mut finished = Vec::new();
    let mut open: Vec<Scope> = Vec::new();
    let mut cursor = tree.walk();

    loop {
        enter(cursor.node(), src, analyzer, &mut open);
        if cursor.goto_first_child() {
            continue;
        }
        // No children: leave this node, then keep leaving parents until one
        // has a next sibling to visit.
        loop {
            leave(cursor.node(), &mut open, &mut finished);
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                finished.sort_by_key(|f: &FunctionMetrics| (f.start_line, f.end_line));
                return finished;
            }
        }
    }
}

fn enter(node: Node, src: &[u8], analyzer: &dyn LanguageAnalyzer, open: &mut Vec<Scope>) {
    // Keyword tokens are anonymous nodes whose kind is their text (the
    // `function` keyword has kind "function"), so only named nodes count.
    if !node.is_named() {
        count_else(node, open);
        return;
    }
    if let Some(kind) = analyzer.function_kind(node) {
        let scope = open_scope(node, kind, src, analyzer, open.last());
        open.push(scope);
        return;
    }
    let Some(scope) = open.last_mut() else {
        return;
    };
    count_decision(node, src, analyzer, scope);
    count_structure(node, analyzer, scope);
}

fn open_scope(
    node: Node,
    kind: &'static str,
    src: &[u8],
    analyzer: &dyn LanguageAnalyzer,
    parent: Option<&Scope>,
) -> Scope {
    let local = analyzer.name(node, src);
    let name = match parent {
        Some(parent) => format!("{}/{}", parent.metrics.name, local),
        None => local,
    };
    let start_line = node.start_position().row + 1;
    let end_line = node.end_position().row + 1;
    Scope {
        node_id: node.id(),
        nesting: Vec::new(),
        metrics: FunctionMetrics {
            name,
            kind,
            start_line,
            end_line,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            complexity: 1,
            lines: end_line - start_line + 1,
            parameters: parameter_count(node),
            ignored: ignore_marker(node, src),
            ..FunctionMetrics::default()
        },
    }
}

/// Logical operators whose sequences add to cognitive complexity.
const LOGICAL: &[&str] = &["&&", "||", "and", "or"];

fn count_decision(node: Node, src: &[u8], analyzer: &dyn LanguageAnalyzer, scope: &mut Scope) {
    let Some(category) = analyzer.decision(node, src) else {
        return;
    };
    let weight = analyzer.weight(node);
    scope.metrics.complexity += weight;
    *scope.metrics.breakdown.entry(category).or_insert(0) += weight;
    // `a && b && c` is one sequence (+1); `a && b || c` is two (+2).
    if LOGICAL.contains(&category) && starts_sequence(node) {
        scope.metrics.cognitive += 1;
    }
}

fn starts_sequence(node: Node) -> bool {
    let operator = |n: Node| n.child_by_field_name("operator").map(|op| op.kind());
    node.parent()
        .is_none_or(|parent| parent.kind() != node.kind() || operator(parent) != operator(node))
}

/// A branch or loop adds 1 plus the current nesting, and nests what is inside.
fn count_structure(node: Node, analyzer: &dyn LanguageAnalyzer, scope: &mut Scope) {
    if analyzer.nests(node) {
        let depth = scope.nesting.len() as u32;
        scope.metrics.cognitive += 1 + depth;
        scope.metrics.max_nesting = scope.metrics.max_nesting.max(depth + 1);
        scope.nesting.push(node.id());
    }
}

/// Every `else` (also of `else if`) adds 1 without a nesting penalty. The
/// keyword token is the same in all three grammars, and needs no parent lookup.
fn count_else(node: Node, open: &mut [Scope]) {
    if node.kind() == "else" {
        if let Some(scope) = open.last_mut() {
            scope.metrics.cognitive += 1;
        }
    }
}

fn leave(node: Node, open: &mut Vec<Scope>, finished: &mut Vec<FunctionMetrics>) {
    if let Some(scope) = open.last_mut() {
        scope.nesting.pop_if(|id| *id == node.id());
    }
    if let Some(scope) = open.pop_if(|scope| scope.node_id == node.id()) {
        finished.push(scope.metrics);
    }
}

/// Parameters in the `parameters` field, or 1 for `x => x`.
fn parameter_count(node: Node) -> u32 {
    if node.child_by_field_name("parameter").is_some() {
        return 1;
    }
    let Some(list) = node.child_by_field_name("parameters") else {
        return 0;
    };
    // C# `y => y` has a single `implicit_parameter`, not a list.
    if matches!(list.kind(), "identifier" | "implicit_parameter") {
        return 1;
    }
    let mut cursor = list.walk();
    let count = list
        .named_children(&mut cursor)
        .filter(|child| !is_comment(*child) && !is_attribute(*child))
        .count();
    count as u32
}

fn is_comment(node: Node) -> bool {
    node.kind().contains("comment")
}

fn is_attribute(node: Node) -> bool {
    node.kind().contains("attribute") || node.kind() == "decorator"
}

/// Nodes a function can sit inside while still being "the declaration":
/// `export function f`, `const f = () => ...`, `Func<int> f = x => ...;`.
const WRAPPERS: &[&str] = &[
    "export_statement",
    "variable_declarator",
    "variable_declaration",
    "lexical_declaration",
    "local_declaration_statement",
    "public_field_definition",
    "pair",
    "assignment_expression",
    "expression_statement",
];

/// `// dgf-ignore: reason` directly above a function (attributes and
/// decorators may sit in between) excludes it from the limits.
fn ignore_marker(node: Node, src: &[u8]) -> Option<String> {
    let mut previous = declaration_of(node).prev_sibling();
    while let Some(sibling) = previous {
        if is_comment(sibling) {
            if let Some(reason) = marker(sibling, src) {
                return Some(reason);
            }
        } else if !is_attribute(sibling) {
            return None;
        }
        previous = sibling.prev_sibling();
    }
    None
}

fn declaration_of(node: Node) -> Node {
    let mut anchor = node;
    while let Some(parent) = anchor.parent().filter(|p| WRAPPERS.contains(&p.kind())) {
        anchor = parent;
    }
    anchor
}

/// The comment must start with `dgf-ignore`; a comment that merely mentions
/// it (like this one) does not count.
fn marker(comment: Node, src: &[u8]) -> Option<String> {
    let text = comment.utf8_text(src).ok()?;
    let body = text.trim_start_matches(['/', '*', '#', '!']).trim_start();
    let rest = body.strip_prefix("dgf-ignore")?;
    let reason = rest
        .trim_end_matches("*/")
        .trim()
        .trim_start_matches(':')
        .trim();
    Some(reason.to_string())
}
