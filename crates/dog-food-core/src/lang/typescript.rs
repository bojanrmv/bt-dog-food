use tree_sitter::Node;

use super::{category, field_text, logical_operator, nests, position, LanguageAnalyzer};

/// Node kinds that always add 1, and the category each is reported under.
const DECISIONS: &[(&str, &str)] = &[
    ("if_statement", "if"),
    ("for_statement", "loop"),
    // for_in_statement covers both `for…in` and `for…of`.
    ("for_in_statement", "loop"),
    ("while_statement", "loop"),
    ("do_statement", "loop"),
    // `default:` is a separate switch_default node and does not count.
    ("switch_case", "case"),
    ("catch_clause", "catch"),
    ("ternary_expression", "?:"),
];

/// Covers TypeScript and JavaScript; `tsx` switches to the TSX grammar.
pub(crate) struct TypeScriptAnalyzer {
    tsx: bool,
}

pub(crate) static TYPESCRIPT: TypeScriptAnalyzer = TypeScriptAnalyzer { tsx: false };
pub(crate) static TSX: TypeScriptAnalyzer = TypeScriptAnalyzer { tsx: true };

/// Branches and loops that nest (cognitive complexity).
const NESTING: &[&str] = &[
    "if_statement",
    "for_statement",
    "for_in_statement",
    "while_statement",
    "do_statement",
    "catch_clause",
    "switch_statement",
    "ternary_expression",
];

impl LanguageAnalyzer for TypeScriptAnalyzer {
    fn grammar(&self) -> tree_sitter::Language {
        if self.tsx {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    }

    fn function_kind(&self, node: Node) -> Option<&'static str> {
        match node.kind() {
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "function"
            | "generator_function" => Some("function"),
            "arrow_function" => Some("arrow"),
            "method_definition" => Some("method"),
            _ => None,
        }
    }

    fn decision(&self, node: Node, _src: &[u8]) -> Option<&'static str> {
        if node.kind() == "binary_expression" {
            return logical_operator(node, &["&&", "||", "??"]);
        }
        category(DECISIONS, node.kind())
    }

    fn nests(&self, node: Node) -> bool {
        nests(node, NESTING)
    }

    fn name(&self, node: Node, src: &[u8]) -> String {
        if let Some(name) = field_text(node, "name", src) {
            return match class_name(node, src) {
                Some(class) => format!("{class}.{name}"),
                None => name.to_string(),
            };
        }
        // Anonymous functions take the name of what they are assigned to.
        let assigned = node.parent().and_then(|parent| match parent.kind() {
            "variable_declarator" => field_text(parent, "name", src).map(str::to_string),
            "pair" => field_text(parent, "key", src).map(str::to_string),
            "assignment_expression" => field_text(parent, "left", src).map(str::to_string),
            "public_field_definition" => {
                field_text(parent, "name", src).map(|name| match class_name(parent, src) {
                    Some(class) => format!("{class}.{name}"),
                    None => name.to_string(),
                })
            }
            _ => None,
        });
        assigned.unwrap_or_else(|| format!("<anonymous>@{}", position(node)))
    }
}

/// Name of the class a member is declared directly in.
fn class_name<'a>(member: Node, src: &'a [u8]) -> Option<&'a str> {
    let body = member.parent()?;
    if body.kind() != "class_body" {
        return None;
    }
    let class = body.parent()?;
    match class.kind() {
        "class_declaration" | "abstract_class_declaration" | "class" => {
            field_text(class, "name", src)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{analyze_source, Language};

    fn complexity(src: &str, name: &str) -> u32 {
        let functions = analyze_source(src, Language::TypeScript).unwrap();
        functions
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function `{name}` in {functions:#?}"))
            .complexity
    }

    #[test]
    fn straight_line_function_is_one() {
        assert_eq!(
            complexity(
                "function add(a: number, b: number) { return a + b; }",
                "add"
            ),
            1
        );
    }

    #[test]
    fn each_if_and_else_if_adds_one() {
        let src = "function f(x: number) { if (x < 0) return -1; else if (x === 0) return 0; else return 1; }";
        assert_eq!(complexity(src, "f"), 3);
    }

    #[test]
    fn every_loop_kind_adds_one() {
        let src = "function f(xs: number[]) { for (let i = 0; i < 1; i++) {} for (const x of xs) {} for (const k in xs) {} while (false) {} do {} while (false); }";
        assert_eq!(complexity(src, "f"), 6);
    }

    #[test]
    fn switch_counts_cases_but_not_default() {
        let src = "function f(s: string) { switch (s) { case 'a': return 1; case 'b': return 2; default: return 0; } }";
        assert_eq!(complexity(src, "f"), 3);
    }

    #[test]
    fn logical_operators_ternary_and_catch_add_one_each() {
        let src = "function f(a: any, b: any, c: any) { try { return a && b || (c ?? 1) ? 1 : 0; } catch (e) { return -1; } }";
        // && + || + ?? + ?: + catch
        assert_eq!(complexity(src, "f"), 6);
    }

    #[test]
    fn nested_arrow_functions_are_counted_separately() {
        let src =
            "function outer(a: boolean) { if (a) {} const inner = (b: boolean) => { if (b) {} }; }";
        assert_eq!(complexity(src, "outer"), 2);
        assert_eq!(complexity(src, "outer/inner"), 2);
    }

    #[test]
    fn methods_are_named_after_their_class() {
        let src = "class Cart { total(): number { return 0; } }";
        assert_eq!(complexity(src, "Cart.total"), 1);
    }

    #[test]
    fn tsx_files_parse_with_the_tsx_grammar() {
        let src = "const Badge = (p: { on: boolean }) => <span>{p.on ? 'on' : 'off'}</span>;";
        let functions = analyze_source(src, Language::Tsx).unwrap();
        assert_eq!(functions[0].name, "Badge");
        assert_eq!(functions[0].complexity, 2);
    }

    fn metrics(src: &str, name: &str) -> crate::FunctionMetrics {
        analyze_source(src, Language::TypeScript)
            .unwrap()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function `{name}`"))
    }

    #[test]
    fn cognitive_complexity_grows_with_nesting() {
        // for (+1) > for (+2) > if (+3)
        let src = "function primes(max: number) { for (let i = 1; i < max; i++) { for (let j = 2; j < i; j++) { if (i % j === 0) {} } } }";
        let f = metrics(src, "primes");
        assert_eq!(f.cognitive, 6);
        assert_eq!(f.max_nesting, 3);
    }

    #[test]
    fn else_branches_and_logical_sequences_add_one() {
        // if +1, else if +1, else +1, `a && b && c` +1, `|| d` +1
        let src = "function f(a: boolean, b: boolean, c: boolean, d: boolean) { if (a && b && c || d) {} else if (b) {} else {} }";
        let f = metrics(src, "f");
        assert_eq!(f.cognitive, 5);
        assert_eq!(f.max_nesting, 1);
        assert_eq!(f.parameters, 4);
    }

    #[test]
    fn a_switch_counts_once_for_cognitive_complexity() {
        let src = "function f(s: string) { switch (s) { case 'a': return 1; case 'b': return 2; default: return 0; } }";
        let f = metrics(src, "f");
        assert_eq!((f.complexity, f.cognitive), (3, 1));
    }

    #[test]
    fn dgf_ignore_comment_marks_the_function_below() {
        let src = "// dgf-ignore: generated parser table
export const parse = (x: number) => x;

function other() {}";
        assert_eq!(
            metrics(src, "parse").ignored.as_deref(),
            Some("generated parser table")
        );
        assert_eq!(metrics(src, "parse").parameters, 1);
        assert_eq!(metrics(src, "other").ignored, None);
        assert_eq!(metrics(src, "other").parameters, 0);
    }
}
