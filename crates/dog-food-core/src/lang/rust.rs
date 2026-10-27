use tree_sitter::Node;

use super::{field_text, logical_operator, nests, position, LanguageAnalyzer};

/// Branches and loops that nest (cognitive complexity). `loop` counts here
/// although it has no condition: it is still a structure to follow.
const NESTING: &[&str] = &[
    "if_expression",
    "for_expression",
    "while_expression",
    "loop_expression",
    "match_expression",
];

pub(crate) struct RustAnalyzer;

impl LanguageAnalyzer for RustAnalyzer {
    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn function_kind(&self, node: Node) -> Option<&'static str> {
        match node.kind() {
            "function_item" if owner_node(node).is_some() => Some("method"),
            "function_item" => Some("function"),
            "closure_expression" => Some("closure"),
            _ => None,
        }
    }

    fn decision(&self, node: Node, _src: &[u8]) -> Option<&'static str> {
        match node.kind() {
            // `else if` is a nested if_expression, so it counts too.
            "if_expression" => Some("if"),
            // `loop` has no condition, so it is not a decision.
            "while_expression" | "for_expression" => Some("loop"),
            // A match with n arms adds n - 1 paths.
            "match_arm" if has_later_arm(node) => Some("match arm"),
            "binary_expression" => logical_operator(node, &["&&", "||"]),
            _ => None,
        }
    }

    fn nests(&self, node: Node) -> bool {
        nests(node, NESTING)
    }

    fn name(&self, node: Node, src: &[u8]) -> String {
        if node.kind() == "closure_expression" {
            return format!("closure@{}", position(node));
        }
        let name = field_text(node, "name", src).unwrap_or("<anonymous>");
        match owner_node(node).and_then(|owner| owner_name(owner, src)) {
            Some(owner) => format!("{owner}::{name}"),
            None => name.to_string(),
        }
    }
}

/// The `impl` or `trait` block a function is declared directly in.
fn owner_node(node: Node) -> Option<Node> {
    let body = node.parent()?;
    if body.kind() != "declaration_list" {
        return None;
    }
    let owner = body.parent()?;
    matches!(owner.kind(), "impl_item" | "trait_item").then_some(owner)
}

fn owner_name<'a>(owner: Node, src: &'a [u8]) -> Option<&'a str> {
    match owner.kind() {
        "impl_item" => field_text(owner, "type", src),
        _ => field_text(owner, "name", src),
    }
}

fn has_later_arm(arm: Node) -> bool {
    let mut next = arm.next_named_sibling();
    while let Some(sibling) = next {
        if sibling.kind() == "match_arm" {
            return true;
        }
        next = sibling.next_named_sibling();
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::{analyze_source, Language};

    fn complexity(src: &str, name: &str) -> u32 {
        let functions = analyze_source(src, Language::Rust).unwrap();
        functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function `{name}` in {functions:#?}"))
            .complexity
    }

    #[test]
    fn straight_line_function_is_one() {
        assert_eq!(
            complexity("fn add(a: i32, b: i32) -> i32 { a + b }", "add"),
            1
        );
    }

    #[test]
    fn each_if_and_else_if_adds_one() {
        let src = "fn f(x: i32) -> i32 { if x < 0 { -1 } else if x == 0 { 0 } else { 1 } }";
        assert_eq!(complexity(src, "f"), 3);
    }

    #[test]
    fn while_and_for_add_one_but_loop_does_not() {
        let src = "fn f(v: &[i32]) { for _ in v {} while false {} loop { break; } }";
        assert_eq!(complexity(src, "f"), 3);
    }

    #[test]
    fn match_adds_arms_minus_one() {
        let src =
            "fn f(x: Option<i32>) -> i32 { match x { Some(0) => 0, Some(n) => n, None => -1 } }";
        assert_eq!(complexity(src, "f"), 3);
    }

    #[test]
    fn each_logical_operator_adds_one() {
        let src = "fn f(a: bool, b: bool, c: bool) -> bool { a && b || c }";
        assert_eq!(complexity(src, "f"), 3);
    }

    #[test]
    fn closures_are_counted_separately() {
        let src = "fn f(v: &[i32]) -> usize {\n    v.iter().filter(|x| if **x > 0 { true } else { false }).count()\n}";
        assert_eq!(complexity(src, "f"), 1);
        assert_eq!(complexity(src, "f/closure@2:21"), 2);
    }

    #[test]
    fn methods_are_named_after_their_impl() {
        let src = "struct Cart; impl Cart { fn total(&self) -> u32 { 0 } }";
        assert_eq!(complexity(src, "Cart::total"), 1);
    }

    fn metrics(src: &str, name: &str) -> crate::FunctionMetrics {
        analyze_source(src, Language::Rust)
            .unwrap()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function `{name}`"))
    }

    #[test]
    fn else_if_chains_do_not_nest() {
        // if +1, else if +1, else +1; the nested `if` inside the first branch is +2
        let src = "fn f(x: i32) -> i32 { if x < 0 { if x < -9 { return -2; } -1 } else if x == 0 { 0 } else { 1 } }";
        let f = metrics(src, "f");
        assert_eq!(f.cognitive, 5);
        assert_eq!(f.max_nesting, 2);
    }

    #[test]
    fn loop_and_match_nest() {
        // loop +1, match +2
        let src = "fn f(v: Option<i32>) { loop { match v { Some(_) => break, None => {} } } }";
        assert_eq!(metrics(src, "f").cognitive, 3);
    }

    #[test]
    fn a_comment_that_only_mentions_dgf_ignore_does_not_count() {
        let src = "/// Put `// dgf-ignore` above a function to skip it.\nfn f() {}";
        assert_eq!(metrics(src, "f").ignored, None);
    }

    #[test]
    fn dgf_ignore_skips_attributes_and_counts_parameters() {
        let src = "/* dgf-ignore */
#[inline]
fn f(&self, a: u8, b: u8) {}";
        let f = metrics(src, "f");
        assert_eq!(f.ignored.as_deref(), Some(""));
        assert_eq!(f.parameters, 3);
        assert_eq!(f.lines, 1);
    }
}
