//! Rust: listed by `dgf mutate`, not run. Operators, literals, `!` and `if`
//! conditions, one replacement each.

use tree_sitter::Node;

use super::{text, Found, MethodFilter, Rules, Site};

pub struct Rust;

const OPERATORS: &[(&str, &str, &str)] = &[
    ("<", "<=", "relational"),
    ("<=", "<", "relational"),
    (">", ">=", "relational"),
    (">=", ">", "relational"),
    ("==", "!=", "equality"),
    ("!=", "==", "equality"),
    ("+", "-", "arithmetic"),
    ("-", "+", "arithmetic"),
    ("*", "/", "arithmetic"),
    ("/", "*", "arithmetic"),
    ("%", "*", "arithmetic"),
    ("&&", "||", "logical"),
    ("||", "&&", "logical"),
];

/// What makes the mutants of one node kind.
type Mutator = for<'t> fn(Node<'t>, &[u8], &mut Vec<Found<'t>>);

const MUTATORS: &[(&str, Mutator)] = &[
    ("binary_expression", binary),
    ("boolean_literal", boolean),
    ("unary_expression", negation),
    ("if_expression", condition),
];

fn binary<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(op) = node.child_by_field_name("operator") else {
        return;
    };
    if let Some((_, to, mutator)) = OPERATORS.iter().find(|(from, _, _)| *from == op.kind()) {
        found.push(Found::part(mutator, node, op, *to));
    }
}

fn boolean<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let flipped = if text(node, src) == "true" {
        "false"
    } else {
        "true"
    };
    found.push(Found::new("boolean", node, flipped));
}

fn negation<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    if node.child(0).is_some_and(|c| c.kind() == "!") {
        let operand = node
            .named_child(0)
            .map(|c| text(c, src))
            .unwrap_or_default();
        found.push(Found::new("negation", node, operand));
    }
}

/// The condition of an `if` becomes `false`; `if let` is left alone.
fn condition<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let condition = node
        .child_by_field_name("condition")
        .filter(|c| !c.kind().contains("let"));
    if let Some(condition) = condition {
        found.push(Found::new("condition", condition, "false"));
    }
}

/// Places that only take compile-time constants.
const CONSTANT_ONLY: &[&str] = &["const_item", "static_item", "attribute_item", "parameter"];

impl Rules for Rust {
    fn collect<'t>(&self, node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
        if let Some((_, mutator)) = MUTATORS.iter().find(|(kind, _)| *kind == node.kind()) {
            mutator(node, src, found);
        }
    }

    fn place(&self, found: &Found, _src: &[u8]) -> Option<Site> {
        let mut current = found.site_node.parent();
        while let Some(parent) = current {
            if CONSTANT_ONLY.contains(&parent.kind()) {
                return None;
            }
            current = parent.parent();
        }
        Some(found.site)
    }

    fn ignored(&self, _found: &Found, _src: &[u8], _methods: &MethodFilter) -> Option<String> {
        None
    }

    fn drops_when_covering(&self) -> &'static str {
        ""
    }
}
