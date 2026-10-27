use tree_sitter::Node;

use super::{category, field_text, logical_operator, nests, position, LanguageAnalyzer};

pub(crate) struct CSharpAnalyzer;

/// Node kinds that start a function scope, and the kind each is reported as.
const FUNCTIONS: &[(&str, &str)] = &[
    ("method_declaration", "method"),
    ("constructor_declaration", "constructor"),
    ("destructor_declaration", "destructor"),
    ("operator_declaration", "operator"),
    ("conversion_operator_declaration", "operator"),
    ("accessor_declaration", "accessor"),
    ("local_function_statement", "function"),
    ("lambda_expression", "lambda"),
    ("anonymous_method_expression", "lambda"),
];

/// Node kinds that always add 1, and the category each is reported under.
const DECISIONS: &[(&str, &str)] = &[
    ("if_statement", "if"),
    ("for_statement", "loop"),
    ("foreach_statement", "loop"),
    ("while_statement", "loop"),
    ("do_statement", "loop"),
    ("catch_clause", "catch"),
    ("conditional_expression", "?:"),
    // `x is > 0 and < 10`: pattern combinators branch like && and ||.
    ("and_pattern", "and"),
    ("or_pattern", "or"),
];

/// Branches and loops that nest (cognitive complexity).
const NESTING: &[&str] = &[
    "if_statement",
    "for_statement",
    "foreach_statement",
    "while_statement",
    "do_statement",
    "catch_clause",
    "switch_statement",
    "switch_expression",
    "conditional_expression",
];

/// Declarations whose members are named `Type.Member`.
const TYPES: &[&str] = &[
    "class_declaration",
    "struct_declaration",
    "record_declaration",
    "interface_declaration",
];

impl LanguageAnalyzer for CSharpAnalyzer {
    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }

    fn prepare(&self, source: &str) -> Vec<String> {
        let modern = super::prepare::csharp(source);
        let first = super::prepare::first_branches(&modern);
        vec![modern, first]
    }

    fn function_kind(&self, node: Node) -> Option<&'static str> {
        if is_expression_bodied_property(node) {
            return Some("accessor");
        }
        category(FUNCTIONS, node.kind())
    }

    fn decision(&self, node: Node, _src: &[u8]) -> Option<&'static str> {
        match node.kind() {
            "binary_expression" => logical_operator(node, &["&&", "||", "??"]),
            // `default:` has no `case` label, so a section of only `default` adds nothing.
            "switch_section" if case_labels(node) > 0 => Some("case"),
            // A switch expression with n arms adds n - 1 paths, like a Rust match.
            "switch_expression_arm" if has_later_arm(node) => Some("switch arm"),
            kind => category(DECISIONS, kind),
        }
    }

    fn weight(&self, node: Node) -> u32 {
        if node.kind() == "switch_section" {
            case_labels(node)
        } else {
            1
        }
    }

    fn nests(&self, node: Node) -> bool {
        nests(node, NESTING)
    }

    fn name(&self, node: Node, src: &[u8]) -> String {
        let local = local_name(node, src);
        match member_owner(node, src) {
            Some(owner) => format!("{owner}.{local}"),
            None => local,
        }
    }
}

/// `int Total => a + b;` has no accessor nodes; its arrow body is the getter.
fn is_expression_bodied_property(node: Node) -> bool {
    node.kind() == "arrow_expression_clause"
        && node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "property_declaration" | "indexer_declaration"
            )
        })
}

/// Name of a function without its type, e.g. `Run`, `operator +`, `get`.
fn local_name(node: Node, src: &[u8]) -> String {
    match node.kind() {
        "operator_declaration" | "conversion_operator_declaration" => operator_name(node, src),
        "destructor_declaration" => format!("~{}", declared_name(node, src)),
        // An expression-bodied property has no keyword: it is the getter.
        "accessor_declaration" | "arrow_expression_clause" => {
            accessor_keyword(node).unwrap_or("get").to_string()
        }
        "lambda_expression" | "anonymous_method_expression" => lambda_name(node, src),
        _ => declared_name(node, src).to_string(),
    }
}

fn declared_name<'a>(node: Node, src: &'a [u8]) -> &'a str {
    field_text(node, "name", src).unwrap_or("<anonymous>")
}

/// `operator +` or, for conversions, `operator int`.
fn operator_name(node: Node, src: &[u8]) -> String {
    let field = if node.kind() == "operator_declaration" {
        "operator"
    } else {
        "type"
    };
    format!("operator {}", field_text(node, field, src).unwrap_or("?"))
}

fn lambda_name(node: Node, src: &[u8]) -> String {
    assigned_name(node, src)
        .map(str::to_string)
        .unwrap_or_else(|| format!("lambda@{}", position(node)))
}

/// `get`, `set`, `init`, `add` or `remove`.
fn accessor_keyword(accessor: Node) -> Option<&'static str> {
    let mut cursor = accessor.walk();
    let keyword = accessor.children(&mut cursor).find_map(|child| {
        ["get", "set", "init", "add", "remove"]
            .into_iter()
            .find(|k| *k == child.kind())
    });
    keyword
}

/// The variable a lambda is assigned to, as in `var f = x => x;`.
fn assigned_name<'a>(node: Node, src: &'a [u8]) -> Option<&'a str> {
    let parent = node.parent()?;
    (parent.kind() == "variable_declarator")
        .then(|| field_text(parent, "name", src))
        .flatten()
}

/// What a member belongs to: `Cart` for a method, `Cart.Count` for a
/// property accessor, `Outer.Inner` for a member of a nested type.
fn member_owner(node: Node, src: &[u8]) -> Option<String> {
    let mut parent = node.parent()?;
    let mut property = None;
    if matches!(
        parent.kind(),
        "accessor_list" | "property_declaration" | "indexer_declaration"
    ) {
        let declaration = if parent.kind() == "accessor_list" {
            parent.parent()?
        } else {
            parent
        };
        property = Some(match declaration.kind() {
            "indexer_declaration" => "this",
            _ => field_text(declaration, "name", src)?,
        });
        parent = declaration.parent()?;
    }
    if parent.kind() != "declaration_list" {
        return None;
    }
    let mut owner = type_path(parent.parent()?, src)?;
    if let Some(property) = property {
        owner = format!("{owner}.{property}");
    }
    Some(owner)
}

/// `Outer.Inner` for a type declaration, following enclosing types outwards.
fn type_path(declaration: Node, src: &[u8]) -> Option<String> {
    if !TYPES.contains(&declaration.kind()) {
        return None;
    }
    let name = field_text(declaration, "name", src)?;
    let outer = declaration
        .parent()
        .filter(|list| list.kind() == "declaration_list")
        .and_then(|list| list.parent())
        .and_then(|outer| type_path(outer, src));
    Some(match outer {
        Some(outer) => format!("{outer}.{name}"),
        None => name.to_string(),
    })
}

fn case_labels(section: Node) -> u32 {
    let mut cursor = section.walk();
    let count = section
        .children(&mut cursor)
        .filter(|child| child.kind() == "case")
        .count();
    count as u32
}

fn has_later_arm(arm: Node) -> bool {
    let mut next = arm.next_named_sibling();
    while let Some(sibling) = next {
        if sibling.kind() == "switch_expression_arm" {
            return true;
        }
        next = sibling.next_named_sibling();
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::{analyze_source, FunctionMetrics, Language};

    fn functions(src: &str) -> Vec<FunctionMetrics> {
        analyze_source(src, Language::CSharp).unwrap()
    }

    fn complexity(src: &str, name: &str) -> u32 {
        let functions = functions(src);
        functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function `{name}` in {functions:#?}"))
            .complexity
    }

    /// Wraps statements in `class C { int M(object o, int x) { ... } }`.
    fn method(body: &str) -> String {
        format!("class C {{ int M(object o, int x, int[] xs) {{ {body} }} }}")
    }

    #[test]
    fn straight_line_method_is_one() {
        assert_eq!(complexity(&method("return x;"), "C.M"), 1);
    }

    #[test]
    fn each_if_and_else_if_adds_one() {
        let src = method("if (x < 0) return -1; else if (x == 0) return 0; else return 1;");
        assert_eq!(complexity(&src, "C.M"), 3);
    }

    #[test]
    fn every_loop_kind_adds_one() {
        let src = method(
            "for (;;) {} foreach (var i in xs) {} while (false) {} do {} while (false); return 0;",
        );
        assert_eq!(complexity(&src, "C.M"), 5);
    }

    #[test]
    fn switch_counts_each_case_label_but_not_default() {
        let src =
            method("switch (x) { case 1: case 2: return 1; case 3: return 2; default: return 0; }");
        assert_eq!(complexity(&src, "C.M"), 4);
    }

    #[test]
    fn switch_expression_adds_arms_minus_one() {
        let src = method("return o switch { int n when n > 0 => 1, string => 2, _ => 3 };");
        assert_eq!(complexity(&src, "C.M"), 3);
    }

    #[test]
    fn catch_and_ternary_add_one_each() {
        let src = method("try { } catch (System.Exception) { } catch { } return x > 0 ? 1 : 0;");
        assert_eq!(complexity(&src, "C.M"), 4);
    }

    #[test]
    fn logical_and_null_coalescing_count_but_null_conditional_does_not() {
        let src = method(
            "var s = o?.ToString(); return (x > 0 && x < 9 || x == 20) ? 1 : (int?)null ?? 0;",
        );
        // && + || + ?: + ??
        assert_eq!(complexity(&src, "C.M"), 5);
    }

    #[test]
    fn pattern_and_or_add_one_each() {
        let src = method("return (o is int and > 0 or < -5) ? 1 : 0;");
        // and + or + ?:
        assert_eq!(complexity(&src, "C.M"), 4);
    }

    #[test]
    fn lambdas_and_local_functions_are_counted_separately() {
        let src = method("Func<int, int> abs = y => y > 0 ? y : -y; int Local(int z) { if (z > 0) {} return z; } return 0;");
        assert_eq!(complexity(&src, "C.M"), 1);
        assert_eq!(complexity(&src, "C.M/abs"), 2);
        assert_eq!(complexity(&src, "C.M/Local"), 2);
    }

    #[test]
    fn members_are_named_after_their_type() {
        let src = "namespace Shop; class Cart { \
            Cart(int x) { } ~Cart() { } \
            int Count { get { return 0; } set { if (value > 0) { } } } \
            int Total => a > 0 ? 1 : 0; \
            int this[int i] { get => i; } \
            static Cart operator +(Cart a, Cart b) => a; \
            class Line { void Add() { } } }";
        let names: Vec<_> = functions(src)
            .into_iter()
            .map(|f| (f.name, f.complexity))
            .collect();
        for expected in [
            ("Cart.Cart", 1),
            ("Cart.~Cart", 1),
            ("Cart.Count.get", 1),
            ("Cart.Count.set", 2),
            ("Cart.Total.get", 2),
            ("Cart.this.get", 1),
            ("Cart.operator +", 1),
            ("Cart.Line.Add", 1),
        ] {
            assert!(
                names
                    .iter()
                    .any(|(name, c)| name == expected.0 && *c == expected.1),
                "missing {expected:?} in {names:?}"
            );
        }
    }

    #[test]
    fn records_structs_and_interfaces_name_their_members() {
        let src = "record Point(int X, int Y) { public int Sum() => X + Y; } struct S { void M() { } } interface I { void N() { } }";
        let names: Vec<_> = functions(src).into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["Point.Sum", "S.M", "I.N"]);
    }

    fn metrics(src: &str, name: &str) -> FunctionMetrics {
        functions(src)
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function `{name}`"))
    }

    #[test]
    fn cognitive_complexity_for_else_if_chains_and_catch() {
        // if +1, else if +1, else +1, try/catch: catch +1, nested if in catch +2
        let src = method("if (x < 0) return 1; else if (x == 0) return 2; else return 3; try { } catch { if (x > 0) { } }");
        let f = metrics(&src, "C.M");
        assert_eq!(f.cognitive, 6);
        assert_eq!(f.max_nesting, 2);
        assert_eq!(f.parameters, 3);
    }

    #[test]
    fn lambda_parameters_and_ignore_marker() {
        let src = "class C { // dgf-ignore: legacy, see #42
 [Obsolete] void M() { Func<int, int> f = y => y; } }";
        assert_eq!(
            metrics(src, "C.M").ignored.as_deref(),
            Some("legacy, see #42")
        );
        assert_eq!(metrics(src, "C.M/f").parameters, 1);
    }
}
