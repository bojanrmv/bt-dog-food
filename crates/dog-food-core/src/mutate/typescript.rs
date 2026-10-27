//! TypeScript and JavaScript. At Basic only the operator mutators run. The
//! sandbox turns off type checks in mutated files (`// @ts-nocheck`), so a
//! mutant only has to be valid syntax.

use tree_sitter::Node;

use super::{compact, literal, pattern, text, Found, MethodFilter, Rules, Site};

pub struct TypeScript;

impl Rules for TypeScript {
    fn collect<'t>(&self, node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
        let mutators = MUTATORS.iter().filter(|(kind, _)| *kind == node.kind());
        for (_, mutator) in mutators {
            if !skipped(node) {
                mutator(node, src, found);
            }
        }
    }

    fn place(&self, found: &Found, src: &[u8]) -> Option<Site> {
        let (mut node, mut site) = (found.site_node, found.site);
        if !site.statement {
            (node, site) = expression_site(node, site)?;
        }
        let placeable = (!site.statement || wrappable(node))
            && !in_fixed_place(node)
            && !holds_tagged_lines(node, site, found)
            && !replacement_breaks(node, found);
        let multiline = !fits_one_line(node, site, found, src);
        placeable.then_some(Site { multiline, ..site })
    }

    fn ignored(&self, found: &Found, src: &[u8], methods: &MethodFilter) -> Option<String> {
        let mut current = match found.node.kind() {
            "expression_statement" => found.node.named_child(0),
            _ => Some(found.node),
        };
        while let Some(node) = current.filter(|_| !methods.is_empty()) {
            if ignored_call(node, src, methods) {
                return Some("inside a call that ignore_methods names".into());
            }
            current = node.parent().filter(|_| !is_statement_boundary(node));
        }
        None
    }

    fn drops_when_covering(&self) -> &'static str {
        "call"
    }
}

/// What makes the mutants of one node kind.
type Mutator = for<'t> fn(Node<'t>, &[u8], &mut Vec<Found<'t>>);

const MUTATORS: &[(&str, Mutator)] = &[
    ("binary_expression", binary),
    ("true", boolean),
    ("false", boolean),
    ("unary_expression", unary),
    ("update_expression", update),
    ("augmented_assignment_expression", assignment),
    ("if_statement", condition),
    ("while_statement", condition),
    ("do_statement", condition),
    ("for_statement", condition),
    ("ternary_expression", condition),
    ("string", string),
    ("template_string", string),
    ("array", array),
    ("new_expression", array_constructor),
    ("regex", regex_literal),
    ("new_expression", regex_constructor),
    ("call_expression", array_constructor),
    ("call_expression", method),
    ("call_expression", optional),
    ("object", object),
    ("arrow_function", arrow),
    ("member_expression", optional),
    ("subscript_expression", optional),
    ("statement_block", block),
    ("expression_statement", call_statement),
    ("throw_statement", throw_statement),
    ("switch_case", emptied_case),
    ("switch_default", emptied_case),
];

fn boolean<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let flipped = if node.kind() == "true" {
        "false"
    } else {
        "true"
    };
    found.push(Found::new("boolean", node, flipped));
}

fn update<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    if let Some(op) = operator(node) {
        let flipped = if op.kind() == "++" { "--" } else { "++" };
        found.push(Found::part("update", node, op, flipped));
    }
}

fn array<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let replacement = if node.named_child_count() == 0 {
        "[\"dgf was here\"]"
    } else {
        "[]"
    };
    found.push(Found::new("array", node, replacement));
}

fn object<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    if node.named_child_count() > 0 {
        found.push(Found::new("object", node, "{}"));
    }
}

/// ArrowFunction: `() => a` becomes `() => undefined`.
fn arrow<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let body = node
        .child_by_field_name("body")
        .filter(|b| b.kind() != "statement_block" && text(*b, src) != "undefined");
    if let Some(body) = body {
        found.push(Found::part("arrow", node, body, "undefined"));
    }
}

/// CallExpression: `save();` becomes `;` (never `super()`).
fn call_statement<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let call = node.named_child(0).is_some_and(|e| {
        e.kind() == "call_expression"
            && e.child_by_field_name("function")
                .is_some_and(|f| f.kind() != "super")
    });
    if call {
        found.push(Found::new("call", node, ";").switched_as_statement());
    }
}

fn throw_statement<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    found.push(Found::new("call", node, ";").switched_as_statement());
}

/// A call whose callee `ignore_methods` names.
fn ignored_call(node: Node, src: &[u8], methods: &MethodFilter) -> bool {
    node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .is_some_and(|f| methods.matches(&compact(f, src).replace("?.", ".")))
}

/// Where the walk for an ignored call stops: a statement other than an
/// expression statement.
fn is_statement_boundary(node: Node) -> bool {
    node.kind().ends_with("statement") && node.kind() != "expression_statement"
}

/// Moves an expression switch past the end of a `?.` chain, and around
/// the whole statement when it would start one: a line starting with `(`
/// can join the line before.
fn expression_site(mut node: Node, mut site: Site) -> Option<(Node, Site)> {
    while continues_chain(node) {
        node = node.parent()?;
        site = Site::of(node);
    }
    let statement = enclosing_expression_statement(node).filter(|s| s.start_byte() == site.start);
    Some(match statement {
        Some(statement) => (statement, Site::statement(statement)),
        None => (node, site),
    })
}

/// Nodes never mutated: types, imports, decorators, enums, `as`
/// expressions, ambient declarations.
fn skipped(node: Node) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        let kind = n.kind();
        if kind.contains("type")
            || matches!(
                kind,
                "import_statement"
                    | "decorator"
                    | "enum_declaration"
                    | "as_expression"
                    | "ambient_declaration"
                    | "interface_declaration"
                    | "function_signature"
            )
        {
            return true;
        }
        current = n.parent();
    }
    false
}

fn operator(node: Node) -> Option<Node> {
    node.child_by_field_name("operator").or_else(|| {
        let mut cursor = node.walk();
        let found = node
            .children(&mut cursor)
            .find(|c| matches!(c.kind(), "++" | "--"));
        found
    })
}

const OPERATORS: &[(&str, &[&str], &str)] = &[
    ("+", &["-"], "arithmetic"),
    ("-", &["+"], "arithmetic"),
    ("*", &["/"], "arithmetic"),
    ("/", &["*"], "arithmetic"),
    ("%", &["*"], "arithmetic"),
    ("<", &["<=", ">="], "relational"),
    ("<=", &["<", ">"], "relational"),
    (">", &[">=", "<="], "relational"),
    (">=", &[">", "<"], "relational"),
    ("==", &["!="], "equality"),
    ("!=", &["=="], "equality"),
    ("===", &["!=="], "equality"),
    ("!==", &["==="], "equality"),
    ("&&", &["||"], "logical"),
    ("||", &["&&"], "logical"),
    ("??", &["&&"], "logical"),
];

fn binary<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(op) = node.child_by_field_name("operator") else {
        return;
    };
    if is_arithmetic(op.kind()) && has_text_operand(node) {
        return;
    }
    operators(node, op, found);
    if is_boolean(op.kind()) && !is_condition_test(node) {
        boolean_expression(node, found);
    }
}

fn operators<'t>(node: Node<'t>, op: Node<'t>, found: &mut Vec<Found<'t>>) {
    let Some((_, replacements, mutator)) = OPERATORS.iter().find(|(o, _, _)| *o == op.kind())
    else {
        return;
    };
    for replacement in *replacements {
        found.push(Found::part(mutator, node, op, *replacement));
    }
}

fn is_arithmetic(op: &str) -> bool {
    matches!(op, "+" | "-" | "*" | "/" | "%")
}

fn is_boolean(op: &str) -> bool {
    matches!(
        op,
        "==" | "!=" | "===" | "!==" | "<" | "<=" | ">" | ">=" | "&&" | "||"
    )
}

/// Arithmetic with a string or template literal operand is left alone.
fn has_text_operand(node: Node) -> bool {
    ["left", "right"].iter().any(|field| {
        node.child_by_field_name(field)
            .is_some_and(|n| matches!(n.kind(), "string" | "template_string"))
    })
}

/// ConditionalExpression: a boolean expression becomes `true` and `false`,
/// only `false` inside `||`, only `true` inside `&&`.
fn boolean_expression<'t>(node: Node<'t>, found: &mut Vec<Found<'t>>) {
    let parent_op = node
        .parent()
        .filter(|p| p.kind() == "binary_expression")
        .and_then(|p| p.child_by_field_name("operator"))
        .map(|o| o.kind());
    if parent_op != Some("&&") {
        found.push(Found::new("condition", node, "false"));
    }
    if parent_op != Some("||") {
        found.push(Found::new("condition", node, "true"));
    }
}

/// Whether `node` is the test of an `if`, loop or `?:`, mutated there.
fn is_condition_test(node: Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "parenthesized_expression" => current = parent,
            "if_statement" | "while_statement" | "do_statement" | "for_statement"
            | "ternary_expression" => {
                return parent.child_by_field_name("condition") == Some(current);
            }
            "expression_statement" => {
                return parent.parent().is_some_and(|f| {
                    f.kind() == "for_statement"
                        && f.child_by_field_name("condition") == Some(parent)
                });
            }
            _ => return false,
        }
    }
    false
}

fn unary<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let (Some(op), Some(argument)) = (
        node.child_by_field_name("operator"),
        node.child_by_field_name("argument"),
    ) else {
        return;
    };
    match op.kind() {
        "!" => found.push(Found::new("negation", node, text(argument, src))),
        "~" => found.push(Found::new("unary", node, text(argument, src))),
        "-" => found.push(Found::part("unary", node, op, "+")),
        "+" => found.push(Found::part("unary", node, op, "-")),
        _ => {}
    }
}

const ASSIGNMENTS: &[(&str, &str)] = &[
    ("+=", "-="),
    ("-=", "+="),
    ("*=", "/="),
    ("/=", "*="),
    ("%=", "*="),
    ("<<=", ">>="),
    (">>=", "<<="),
    ("&=", "|="),
    ("|=", "&="),
    ("&&=", "||="),
    ("||=", "&&="),
    ("??=", "&&="),
];

fn assignment<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(op) = node.child_by_field_name("operator") else {
        return;
    };
    let logical = matches!(op.kind(), "&&=" | "||=" | "??=");
    let text_right = node
        .child_by_field_name("right")
        .is_some_and(|r| matches!(r.kind(), "string" | "template_string"));
    if text_right && !logical {
        return;
    }
    if let Some((_, to)) = ASSIGNMENTS.iter().find(|(from, _)| *from == op.kind()) {
        found.push(Found::part("assignment", node, op, *to));
    }
}

/// ConditionalExpression on tests: an `if` or `?:` test becomes `true` and
/// `false`, a loop test `false`; `for (;;)` becomes `for (;false;)`.
fn condition<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(test) = node.child_by_field_name("condition") else {
        return;
    };
    let test = match test.kind() {
        "expression_statement" => test.named_child(0).unwrap_or(test),
        "empty_statement" => {
            let mut empty = Found::new("condition", test, "false;");
            empty.site = Site::statement(node);
            empty.site_node = node;
            found.push(empty);
            return;
        }
        _ => test,
    };
    let inner = if test.kind() == "parenthesized_expression" {
        test.named_child(0).unwrap_or(test)
    } else {
        test
    };
    let looping = matches!(
        node.kind(),
        "while_statement" | "do_statement" | "for_statement"
    );
    if !looping {
        found.push(Found::new("condition", inner, "true"));
    }
    found.push(Found::new("condition", inner, "false"));
}

fn string<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    if node
        .parent()
        .is_none_or(|parent| string_left_alone(node, parent, src))
    {
        return;
    }
    let template = node.kind() == "template_string";
    let empty = node.named_child_count() == 0;
    let replacement = match (template, empty) {
        (true, true) => "`dgf was here!`",
        (true, false) => "``",
        (false, true) => "\"dgf was here!\"",
        (false, false) => "\"\"",
    };
    found.push(Found::new("string", node, replacement));
}

/// Strings left alone: module names, directives, JSX
/// attributes, property keys, and `require`, `import` and `Symbol` arguments.
fn string_left_alone(node: Node, parent: Node, src: &[u8]) -> bool {
    match parent.kind() {
        "import_statement" | "export_statement" | "expression_statement" | "jsx_attribute" => true,
        "pair" | "public_field_definition" | "method_definition" | "pair_pattern" => {
            parent.child_by_field_name("key") == Some(node)
                || parent.child_by_field_name("name") == Some(node)
        }
        "arguments" => parent
            .parent()
            .is_some_and(|call| is_module_call(call, src)),
        _ => false,
    }
}

fn is_module_call(call: Node, src: &[u8]) -> bool {
    call.kind() == "call_expression"
        && call
            .child_by_field_name("function")
            .is_some_and(|f| matches!(text(f, src), "require" | "import" | "Symbol"))
}

/// The regex mutator on a regex literal.
/// An emptied pattern is written `(?:)`, as `//` would start a comment.
fn regex_literal<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let flags = node
        .child_by_field_name("flags")
        .map_or("", |f| text(f, src));
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return;
    };
    for changed in regex_mutations(text(pattern, src), flags) {
        let body = if changed.is_empty() { "(?:)" } else { &changed };
        found.push(Found::new("regex", node, format!("/{body}/{flags}")));
    }
}

/// The same on the string pattern of `new RegExp("...", flags)`.
fn regex_constructor<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let is_regexp = node
        .child_by_field_name("constructor")
        .is_some_and(|c| text(c, src) == "RegExp");
    let Some(arguments) = node.child_by_field_name("arguments").filter(|_| is_regexp) else {
        return;
    };
    let string = |index| {
        arguments
            .named_child(index)
            .filter(|a: &Node| a.kind() == "string")
    };
    let flags = string(1)
        .and_then(|f| literal::javascript_value(text(f, src)))
        .unwrap_or_default();
    let Some(pattern) = string(0) else {
        return;
    };
    let original = text(pattern, src);
    let value = literal::javascript_value(original).unwrap_or_default();
    for changed in regex_mutations(&value, &flags) {
        let replacement = literal::javascript_literal(&changed, original);
        found.push(Found::new("regex", pattern, replacement));
    }
}

/// `v`-mode classes (`[\w--\d]`) are left alone.
fn regex_mutations(pattern: &str, flags: &str) -> Vec<String> {
    if flags.contains('v') {
        return Vec::new();
    }
    let flavor = pattern::Flavor::JavaScript {
        unicode: flags.contains('u'),
    };
    pattern::mutations(pattern, flavor)
        .into_iter()
        .map(|m| m.pattern)
        .collect()
}

/// `new Array(a, b)` and `Array(a, b)` lose their arguments.
fn array_constructor<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let callee = node
        .child_by_field_name("constructor")
        .or_else(|| node.child_by_field_name("function"));
    if !callee.is_some_and(|c| text(c, src) == "Array") {
        return;
    }
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    let replacement = if arguments.named_child_count() > 0 {
        "()"
    } else {
        "([])"
    };
    found.push(Found::part("array", node, arguments, replacement));
}

/// MethodExpression: `x.trim()` becomes `x`, `startsWith` becomes `endsWith`.
const REMOVED_METHODS: &[&str] = &[
    "charAt",
    "filter",
    "reverse",
    "slice",
    "sort",
    "substr",
    "substring",
    "trim",
];

const SWAPPED_METHODS: &[(&str, &str)] = &[
    ("endsWith", "startsWith"),
    ("startsWith", "endsWith"),
    ("every", "some"),
    ("some", "every"),
    ("toLocaleLowerCase", "toLocaleUpperCase"),
    ("toLocaleUpperCase", "toLocaleLowerCase"),
    ("toLowerCase", "toUpperCase"),
    ("toUpperCase", "toLowerCase"),
    ("trimEnd", "trimStart"),
    ("trimStart", "trimEnd"),
    ("min", "max"),
    ("max", "min"),
    ("setFullYear", "setMonth"),
    ("setMonth", "setFullYear"),
    ("setHours", "setMinutes"),
    ("setMinutes", "setHours"),
    ("setSeconds", "setMilliseconds"),
    ("setMilliseconds", "setSeconds"),
    ("setUTCFullYear", "setUTCMonth"),
    ("setUTCMonth", "setUTCFullYear"),
    ("setUTCHours", "setUTCMinutes"),
    ("setUTCMinutes", "setUTCHours"),
    ("setUTCSeconds", "setUTCMilliseconds"),
    ("setUTCMilliseconds", "setUTCSeconds"),
    ("setDate", "setTime"),
    ("setTime", "setDate"),
    ("setUTCDate", "setTime"),
];

fn method<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(function) = node
        .child_by_field_name("function")
        .filter(|f| f.kind() == "member_expression")
    else {
        return;
    };
    let (Some(object), Some(property)) = (
        function.child_by_field_name("object"),
        function.child_by_field_name("property"),
    ) else {
        return;
    };
    let name = text(property, src);
    if REMOVED_METHODS.contains(&name) {
        found.push(Found::new("method", node, text(object, src)));
    }
    if let Some((_, to)) = SWAPPED_METHODS.iter().find(|(from, _)| *from == name) {
        found.push(Found::part("method", node, property, *to));
    }
}

/// OptionalChaining: `a?.b` becomes `a.b`, `a?.[i]` `a[i]`, `f?.()` `f()`.
fn optional<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let mut cursor = node.walk();
    let chain = node
        .children(&mut cursor)
        .find(|c| c.kind() == "optional_chain");
    if let Some(chain) = chain {
        let replacement = if node.kind() == "member_expression" {
            "."
        } else {
            ""
        };
        found.push(Found::part("optional", node, chain, replacement));
    }
}

/// BlockStatement: a non-empty block becomes `{}`.
fn block<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let mut cursor = node.walk();
    if !node
        .named_children(&mut cursor)
        .any(|c| !c.kind().contains("comment"))
    {
        return;
    }
    // A constructor that calls `super()` must keep it when the class has
    // parameter properties or initialized fields.
    let constructor = node.parent().filter(|p| {
        p.kind() == "method_definition"
            && p.child_by_field_name("name")
                .is_some_and(|n| text(n, src) == "constructor")
    });
    if constructor.is_some() && contains_super_call(node) {
        return;
    }
    found.push(Found::new("block", node, "{}").switched_as_statement());
}

fn contains_super_call(node: Node) -> bool {
    let mut found = false;
    super::walk(node, &mut |n| {
        found |= n.kind() == "call_expression"
            && n.child_by_field_name("function")
                .is_some_and(|f| f.kind() == "super");
    });
    found
}

/// A `case` with statements loses them, and falls through.
fn emptied_case<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let mut cursor = node.walk();
    let body: Vec<Node> = node
        .children_by_field_name("body", &mut cursor)
        .filter(|c| !c.kind().contains("comment"))
        .collect();
    let (Some(first), Some(last)) = (body.first(), body.last()) else {
        return;
    };
    let Some(switch) = node.parent().and_then(|b| b.parent()) else {
        return;
    };
    let mut emptied = Found::new("condition", node, ";");
    emptied.start = first.start_byte();
    emptied.end = last.end_byte();
    emptied.site = Site::statement(switch);
    emptied.site_node = switch;
    found.push(emptied);
}

/// Whether `node` is followed by more of a `?.` chain.
fn continues_chain(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let continued = match parent.kind() {
        "member_expression" | "subscript_expression" => {
            parent.child_by_field_name("object") == Some(node)
        }
        "call_expression" => parent.child_by_field_name("function") == Some(node),
        "non_null_expression" => true,
        _ => false,
    };
    continued && has_optional_chain(node)
}

fn has_optional_chain(node: Node) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        let mut cursor = n.walk();
        if n.children(&mut cursor)
            .any(|c| c.kind() == "optional_chain")
        {
            return true;
        }
        current = match n.kind() {
            "member_expression" | "subscript_expression" => n.child_by_field_name("object"),
            "call_expression" => n.child_by_field_name("function"),
            "non_null_expression" => n.named_child(0),
            _ => None,
        };
    }
    false
}

fn enclosing_expression_statement(node: Node) -> Option<Node> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "expression_statement" {
            return Some(parent);
        }
        if parent.kind().ends_with("statement") || parent.kind() == "statement_block" {
            return None;
        }
        current = parent.parent();
    }
    None
}

fn wrappable(node: Node) -> bool {
    if node.kind() == "statement_block" {
        return true;
    }
    let declaration = matches!(
        node.kind(),
        "lexical_declaration"
            | "variable_declaration"
            | "function_declaration"
            | "class_declaration"
            | "import_statement"
            | "export_statement"
    );
    !declaration
        && node.parent().is_some_and(|p| {
            matches!(
                p.kind(),
                "statement_block"
                    | "program"
                    | "if_statement"
                    | "else_clause"
                    | "while_statement"
                    | "do_statement"
                    | "for_statement"
                    | "for_in_statement"
                    | "switch_case"
                    | "switch_default"
                    | "labeled_statement"
            )
        })
}

/// Places where an expression cannot become `(switch ? a : b)`: assignment
/// and update targets, destructuring patterns.
fn in_fixed_place(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "assignment_expression" | "augmented_assignment_expression" => {
            parent.child_by_field_name("left") == Some(node)
        }
        "update_expression" => parent.child_by_field_name("argument") == Some(node),
        kind => {
            kind.contains("pattern")
                || kind == "unary_expression"
                    && parent.child(0).is_some_and(|op| op.kind() == "delete")
        }
    }
}

/// Whether the mutated copy goes on one line as it is: not when the site
/// keeps a template string spanning lines (its line breaks become `\n`).
fn fits_one_line(node: Node, site: Site, found: &Found, _src: &[u8]) -> bool {
    let mut fits = true;
    super::walk(node, &mut |n| fits &= !breaks_one_line(n, site, found));
    fits
}

/// A tagged template spanning lines (`` String.raw`...` ``) in the copied
/// part: its line breaks cannot be written as `\n`.
fn holds_tagged_lines(node: Node, site: Site, found: &Found) -> bool {
    let mut holds = false;
    super::walk(node, &mut |n| {
        holds |= breaks_one_line(n, site, found) && super::is_tagged(n);
    });
    holds
}

/// A replacement that copies lines of the source (`a.filter(f)` -> `a`)
/// goes on one line too, which a template string spanning lines or a `//`
/// comment in those lines cannot.
fn replacement_breaks(node: Node, found: &Found) -> bool {
    if !found.replacement.contains('\n') {
        return false;
    }
    let mut breaks = false;
    super::walk(node, &mut |n| {
        let inside = found.start <= n.start_byte() && n.end_byte() <= found.end;
        let spans = n.kind() == "template_string" && n.start_position().row != n.end_position().row;
        breaks |= inside && (spans || n.kind() == "comment");
    });
    breaks
}

fn breaks_one_line(node: Node, site: Site, found: &Found) -> bool {
    let within = |start: usize, end: usize| start <= node.start_byte() && node.end_byte() <= end;
    within(site.start, site.end)
        && !within(found.start, found.end)
        && node.kind() == "template_string"
        && node.start_position().row != node.end_position().row
}
