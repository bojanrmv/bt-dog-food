//! C#. There is no type information here, so the rules skip the mutants that
//! certainly would not compile: each failed build costs a full rebuild.

use tree_sitter::Node;

use super::{compact, text, Condition, Found, MethodFilter, Rules, Site};

pub struct CSharp;

impl Rules for CSharp {
    fn collect<'t>(&self, node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
        for (_, mutator) in MUTATORS.iter().filter(|(kind, _)| *kind == node.kind()) {
            mutator(node, src, found);
        }
    }

    fn place(&self, found: &Found, src: &[u8]) -> Option<Site> {
        if let Some((node, site)) = condition_switch(found, src) {
            return placed(node, site, found, src);
        }
        let (mut node, mut site) = (found.site_node, found.site);
        if !site.statement {
            (node, site) = expression_site(found, node, site, src)?;
        }
        (node, site) = with_declarations(found, node, site, src)?;
        placed(node, site, found, src)
    }

    fn boundary_risk(&self, found: &Found, src: &[u8]) -> bool {
        found.mutator == "relational" && compares_dates(found.node, src)
    }

    fn ignored(&self, found: &Found, src: &[u8], methods: &MethodFilter) -> Option<String> {
        let calls = Calls { src, methods };
        if !methods.is_empty() && in_ignored_call(found.node, &calls, true) {
            return Some("inside a call that ignore_methods names".into());
        }
        excluded_from_coverage(found.node, src)
            .then(|| "in a member marked [ExcludeFromCodeCoverage]".into())
    }
}

/// What makes the mutants of one node kind.
type Mutator = for<'t> fn(Node<'t>, &[u8], &mut Vec<Found<'t>>);

const MUTATORS: &[(&str, Mutator)] = &[
    ("binary_expression", binary),
    ("boolean_literal", boolean),
    ("prefix_unary_expression", prefix),
    ("postfix_unary_expression", postfix),
    ("assignment_expression", assignment),
    ("if_statement", if_statement),
    ("while_statement", negate),
    ("conditional_expression", conditional),
    ("checked_expression", checked),
    ("string_literal", string),
    ("verbatim_string_literal", string),
    ("raw_string_literal", string),
    ("interpolated_string_expression", interpolated),
    ("member_access_expression", string_empty),
    ("invocation_expression", invocation),
    ("object_creation_expression", initializer),
    ("array_creation_expression", initializer),
    ("stackalloc_expression", initializer),
    ("initializer_expression", array_initializer),
    ("object_creation_expression", regex_creation),
    ("invocation_expression", regex_call),
    ("is_pattern_expression", is_pattern),
    ("relational_pattern", relational_pattern),
    ("and_pattern", binary_pattern),
    ("or_pattern", binary_pattern),
    ("block", block),
    ("collection_expression", collection),
    ("return_statement", statement),
    ("break_statement", statement),
    ("continue_statement", statement),
    ("goto_statement", statement),
    ("throw_statement", statement),
    ("yield_statement", statement),
    ("expression_statement", statement),
];

/// Binary operators and their replacements.
const OPERATORS: &[(&str, &[&str], &str)] = &[
    ("+", &["-"], "arithmetic"),
    ("-", &["+"], "arithmetic"),
    ("*", &["/"], "arithmetic"),
    ("/", &["*"], "arithmetic"),
    ("%", &["*"], "arithmetic"),
    (">", &["<", ">="], "relational"),
    ("<", &[">", "<="], "relational"),
    (">=", &["<", ">"], "relational"),
    ("<=", &[">", "<"], "relational"),
    ("==", &["!="], "equality"),
    ("!=", &["=="], "equality"),
    ("&&", &["||"], "logical"),
    ("||", &["&&"], "logical"),
    ("<<", &[">>", ">>>"], "bitwise"),
    (">>", &["<<", ">>>"], "bitwise"),
    (">>>", &["<<", ">>"], "bitwise"),
    ("|", &["&"], "bitwise"),
    ("&", &["|"], "bitwise"),
];

fn binary<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(op) = node.child_by_field_name("operator") else {
        return;
    };
    if adds_text(node, op, src) || leaves_unassigned(node, op, src) {
        return;
    }
    let spine = misparsed_spine(node);
    let mut push = |one: Found<'t>| found.push(on_real_site(one, spine, node));
    match op.kind() {
        "^" => {
            push(Found::part("logical", node, op, "=="));
            push(Found::new(
                "bitwise",
                node,
                format!("~({})", text(node, src)),
            ));
        }
        "??" => coalesce(node, src, &mut push),
        kind => operators(node, op, kind, &mut push),
    }
}

fn operators<'t>(node: Node<'t>, op: Node<'t>, kind: &str, push: &mut dyn FnMut(Found<'t>)) {
    let Some((_, replacements, mutator)) = OPERATORS.iter().find(|(o, _, _)| *o == kind) else {
        return;
    };
    for replacement in *replacements {
        push(Found::part(mutator, node, op, *replacement));
    }
}

/// `"a" + b` or `s + t` on strings: string concatenation is left alone.
fn adds_text(node: Node, op: Node, src: &[u8]) -> bool {
    op.kind() == "+" && has_text_operand(node, src)
}

fn has_text_operand(node: Node, src: &[u8]) -> bool {
    let strings = string_names(node, src);
    ["left", "right"].iter().any(|field| {
        node.child_by_field_name(field)
            .is_some_and(|n| is_stringy(n, &strings, src))
    })
}

/// `o is Order x && x.Total > 5`: swapped to `||`, `x` would be unassigned
/// on the right.
fn leaves_unassigned(node: Node, op: Node, src: &[u8]) -> bool {
    if !matches!(op.kind(), "&&" | "||") {
        return false;
    }
    let (Some(left), Some(right)) = (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) else {
        return false;
    };
    let names = declared(left, src);
    !names.is_empty() && uses(right, &names, src)
}

/// `x is null || y` is read as `x is (null || y)`: the operator's real
/// expression then starts at `x`.
fn on_real_site<'t>(found: Found<'t>, spine: Option<Node<'t>>, node: Node<'t>) -> Found<'t> {
    let Some(is) = spine else {
        return found;
    };
    Found {
        site: Site {
            start: is.start_byte(),
            end: node.end_byte(),
            ..Site::of(node)
        },
        site_node: is,
        ..found
    }
}

/// A string operand: a literal, an interpolated string,
/// a variable the function declares as `string`, or a `+` that has one
/// (`"a" + b + c`). Without types, `a - b` on two strings would only show
/// up as a failed build.
fn is_stringy(node: Node, strings: &[String], src: &[u8]) -> bool {
    match node.kind() {
        "string_literal"
        | "verbatim_string_literal"
        | "raw_string_literal"
        | "interpolated_string_expression" => true,
        "identifier" => strings.iter().any(|s| s == text(node, src)),
        "binary_expression" => is_concatenation(node, strings, src),
        "parenthesized_expression" => node
            .named_child(0)
            .is_some_and(|n| is_stringy(n, strings, src)),
        _ => is_string_type(node, "Empty", src),
    }
}

fn is_concatenation(node: Node, strings: &[String], src: &[u8]) -> bool {
    node.child_by_field_name("operator")
        .is_some_and(|op| op.kind() == "+")
        && ["left", "right"].iter().any(|field| {
            node.child_by_field_name(field)
                .is_some_and(|n| is_stringy(n, strings, src))
        })
}

/// Parameters and locals of the function around `node` that are strings:
/// declared `string`, or `var` set to a string literal.
fn string_names(node: Node, src: &[u8]) -> Vec<String> {
    let Some(function) = enclosing_function(node) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    super::walk(function, &mut |n| names.extend(string_declared(n, src)));
    names
}

/// The string variables a parameter or a declaration declares.
fn string_declared(node: Node, src: &[u8]) -> Vec<String> {
    let typed = node
        .child_by_field_name("type")
        .is_some_and(|t| matches!(text(t, src), "string" | "string?" | "String" | "String?"));
    match node.kind() {
        "parameter" if typed => name_of(node, src).into_iter().collect(),
        "variable_declaration" => {
            let mut cursor = node.walk();
            let names = node
                .named_children(&mut cursor)
                .filter(|d| d.kind() == "variable_declarator" && (typed || has_text_value(*d, src)))
                .filter_map(|d| name_of(d, src))
                .collect();
            names
        }
        _ => Vec::new(),
    }
}

fn name_of(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .map(|id| text(id, src).to_string())
}

fn has_text_value(declarator: Node, src: &[u8]) -> bool {
    declarator_value(declarator).is_some_and(|v| is_stringy(v, &[], src))
}

/// The value a declarator sets (`x = value`), if any.
fn declarator_value(declarator: Node) -> Option<Node> {
    declarator
        .named_child(declarator.named_child_count().saturating_sub(1))
        .filter(|v| Some(*v) != declarator.child_by_field_name("name"))
}

/// `a ?? b`: `b`, and `b ?? a` and `a` when `b` may be null.
fn coalesce<'t>(node: Node<'t>, src: &[u8], push: &mut dyn FnMut(Found<'t>)) {
    let (Some(left), Some(right)) = (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) else {
        return;
    };
    let (left_text, right_text) = (text(left, src), text(right, src));
    // `x ?? y` with `x` an `int?`: `y` is an `int`, so `y ?? x` and `x`
    // alone do not compile.
    let never_null = never_null(right, src) || is_nullable_value(left, src);
    if right.kind() != "throw_expression" {
        if !never_null {
            push(Found::new(
                "nullcoalescing",
                node,
                format!("{right_text} ?? {left_text}"),
            ));
        }
        push(Found::new("nullcoalescing", node, right_text));
    }
    if !never_null || right.kind() == "collection_expression" {
        push(Found::new("nullcoalescing", node, left_text));
    }
}

const VALUE_TYPES: &[&str] = &[
    "int",
    "long",
    "short",
    "byte",
    "sbyte",
    "uint",
    "ulong",
    "ushort",
    "decimal",
    "double",
    "float",
    "bool",
    "char",
    "DateOnly",
    "DateTime",
    "DateTimeOffset",
    "TimeOnly",
    "TimeSpan",
    "Guid",
];

/// Whether `node` is a name the file declares as a nullable value type
/// (`int?`, `DateOnly?`, `Nullable<Guid>`): a parameter, local, field or
/// property.
fn is_nullable_value(node: Node, src: &[u8]) -> bool {
    let name = last_name(node, src);
    if name.is_empty() {
        return false;
    }
    let root = std::iter::successors(Some(node), |n| n.parent())
        .last()
        .unwrap_or(node);
    let mut found = false;
    super::walk(root, &mut |n| {
        let typed = n
            .child_by_field_name("type")
            .is_some_and(|t| is_nullable_value_type(text(t, src)));
        if !typed {
            return;
        }
        let declares = |m: Node| name_of(m, src).as_deref() == Some(name.as_str());
        found |= match n.kind() {
            "parameter" | "property_declaration" => declares(n),
            "variable_declaration" => {
                let mut cursor = n.walk();
                let any = n.named_children(&mut cursor).any(declares);
                any
            }
            _ => false,
        };
    });
    found
}

fn is_nullable_value_type(text: &str) -> bool {
    let inner = text.strip_suffix('?').or_else(|| {
        text.strip_prefix("Nullable<")
            .and_then(|t| t.strip_suffix('>'))
    });
    inner.is_some_and(|t| VALUE_TYPES.contains(&t.trim()))
}

/// Without types: a literal other than `null`, a string, `new`,
/// `string.Empty` or `nameof` is never null; anything else may be.
fn never_null(node: Node, src: &[u8]) -> bool {
    is_value_kind(node.kind())
        || is_string_type(node, "Empty", src)
        || text(node, src).starts_with("nameof(")
}

fn is_value_kind(kind: &str) -> bool {
    kind.ends_with("literal") && kind != "null_literal"
        || kind.contains("string")
        || kind.contains("creation_expression")
}

fn boolean<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    if endless_loop_in_value_function(node, src) {
        return;
    }
    let flipped = if text(node, src) == "true" {
        "false"
    } else {
        "true"
    };
    found.push(Found::new("boolean", node, flipped));
}

/// The `true` of `while (true)` in a function that returns a value: with
/// the loop able to end, the function could end without a value (CS0161).
fn endless_loop_in_value_function(condition: Node, src: &[u8]) -> bool {
    text(condition, src) == "true"
        && condition.parent().is_some_and(|p| {
            matches!(p.kind(), "while_statement" | "do_statement")
                && p.child_by_field_name("condition") == Some(condition)
        })
        && enclosing_function(condition).is_some_and(|f| returns_value(f, src))
}

/// Prefix operators: (operator, mutator, replacement); `None` keeps only
/// the operand.
const PREFIX: &[(&str, &str, Option<&str>)] = &[
    ("!", "negation", None),
    ("~", "unary", None),
    ("-", "unary", Some("+")),
    ("+", "unary", Some("-")),
    ("++", "update", Some("--")),
    ("--", "update", Some("++")),
];

fn prefix<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let (Some(op), Some(operand)) = (node.child(0), node.named_child(0)) else {
        return;
    };
    let Some((_, mutator, replacement)) = PREFIX.iter().find(|(o, _, _)| *o == op.kind()) else {
        return;
    };
    found.push(match replacement {
        Some(to) => Found::part(mutator, node, op, *to),
        None => Found::new(mutator, node, text(operand, src)),
    });
}

fn postfix<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(op) = node.child(node.child_count().saturating_sub(1)) else {
        return;
    };
    let update = PREFIX
        .iter()
        .find(|(o, mutator, _)| *o == op.kind() && *mutator == "update");
    if let Some((_, _, Some(to))) = update {
        found.push(Found::part("update", node, op, *to));
    }
}

const ASSIGNMENTS: &[(&str, &[&str])] = &[
    ("+=", &["-="]),
    ("-=", &["+="]),
    ("*=", &["/="]),
    ("/=", &["*="]),
    ("%=", &["*="]),
    ("&=", &["|=", "^="]),
    ("|=", &["&=", "^="]),
    ("^=", &["|=", "&="]),
    ("<<=", &[">>=", ">>>="]),
    (">>=", &["<<=", ">>>="]),
    (">>>=", &["<<=", ">>="]),
    ("??=", &["="]),
];

fn assignment<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(op) = node.child_by_field_name("operator") else {
        return;
    };
    let Some((_, replacements)) = ASSIGNMENTS.iter().find(|(o, _)| *o == op.kind()) else {
        return;
    };
    if op.kind() == "+=" && has_text_operand(node, src) {
        return;
    }
    for replacement in *replacements {
        found.push(Found::part("assignment", node, op, *replacement));
    }
}

fn if_statement<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    // dgf's own: the condition becomes `false`, so the branch never runs.
    if let Some(condition) = node.child_by_field_name("condition") {
        found.push(Found::new("condition", condition, "false"));
    }
    negate(node, src, found);
}

/// `if (c)` and `while (c)` become `!(c)`,
/// except where another mutator already flips it (`!x`, `==`, `!=`, `is`).
fn negate<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(condition) = node.child_by_field_name("condition") else {
        return;
    };
    if flipped_elsewhere(condition)
        || declares_for(condition, node, src)
        || endless_loop_in_value_function(condition, src)
    {
        return;
    }
    found.push(Found::new(
        "negate",
        condition,
        format!("!({})", text(condition, src)),
    ));
}

fn flipped_elsewhere(node: Node) -> bool {
    match node.kind() {
        "parenthesized_expression" => node.named_child(0).is_some_and(flipped_elsewhere),
        "prefix_unary_expression" => node.child(0).is_some_and(|op| op.kind() == "!"),
        "binary_expression" => node
            .child_by_field_name("operator")
            .is_some_and(|op| matches!(op.kind(), "==" | "!=")),
        "is_pattern_expression" => misparsed_root(node).is_none_or(flipped_elsewhere),
        _ => false,
    }
}

/// Whether `condition` declares a variable that the rest of `statement`
/// uses: flipping it would leave the variable unassigned there.
fn declares_for(condition: Node, statement: Node, src: &[u8]) -> bool {
    let names = declared(condition, src);
    !names.is_empty()
        && statement
            .child_by_field_name("consequence")
            .or_else(|| statement.child_by_field_name("body"))
            .is_some_and(|body| uses(body, &names, src))
}

fn conditional<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(condition) = node.child_by_field_name("condition") else {
        return;
    };
    if has_kind(
        condition,
        &["declaration_pattern", "recursive_pattern", "var_pattern"],
    ) {
        return;
    }
    found.push(Found::part("conditional", node, condition, "true"));
    found.push(Found::part("conditional", node, condition, "false"));
}

fn checked<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let inner = node
        .named_child(0)
        .filter(|_| node.child(0).is_some_and(|c| c.kind() == "checked"));
    if let Some(inner) = inner {
        found.push(Found::new("checked", node, text(inner, src)));
    }
}

fn string<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    if in_regex_or_guid(node, src) {
        return;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    let utf8 = children
        .iter()
        .any(|c| c.kind() == "string_literal_encoding");
    let empty = match node.kind() {
        "verbatim_string_literal" => text(node, src) == "@\"\"",
        _ => !children.iter().any(|c| is_string_content(*c)),
    };
    let value = if empty { "\"dgf was here!\"" } else { "\"\"" };
    let suffix = if utf8 { "u8" } else { "" };
    found.push(Found::new("string", node, format!("{value}{suffix}")));
}

fn is_string_content(child: Node) -> bool {
    let kind = child.kind();
    kind != "string_literal_encoding" && !kind.ends_with("_start") && !kind.ends_with("_end")
}

/// The strings of `new Regex("...")` and `new Guid("...")` are left alone.
fn in_regex_or_guid(node: Node, src: &[u8]) -> bool {
    let creation = node
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .filter(|c| c.kind() == "object_creation_expression");
    creation
        .and_then(|c| c.child_by_field_name("type"))
        .is_some_and(|t| {
            matches!(
                text(t, src),
                "Regex" | "Guid" | "System.Text.RegularExpressions.Regex" | "System.Guid"
            )
        })
}

const REGEX_TYPES: &[&str] = &["Regex", "System.Text.RegularExpressions.Regex"];

/// `Regex`'s static methods that take a pattern second.
const REGEX_METHODS: &[&str] = &[
    "IsMatch",
    "Match",
    "Matches",
    "Replace",
    "Split",
    "Count",
    "EnumerateMatches",
    "EnumerateSplits",
];

/// The regex mutator (advanced level): the pattern of
/// `new Regex(...)`, when it is a string literal.
fn regex_creation<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let is_regex = node
        .child_by_field_name("type")
        .is_some_and(|t| REGEX_TYPES.contains(&text(t, src)));
    if let Some(pattern) = pattern_argument(node, 0, src).filter(|_| is_regex) {
        regex_mutants(pattern, src, found);
    }
}

/// The same for `Regex.IsMatch(input, "...")` and the other static
/// methods.
fn regex_call<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let function = node
        .child_by_field_name("function")
        .filter(|f| f.kind() == "member_access_expression");
    let is_regex = function.is_some_and(|f| {
        let named = |field: &str, names: &[&str]| {
            f.child_by_field_name(field)
                .is_some_and(|n| names.contains(&text(n, src)))
        };
        named("expression", REGEX_TYPES) && named("name", REGEX_METHODS)
    });
    if let Some(pattern) = pattern_argument(node, 1, src).filter(|_| is_regex) {
        regex_mutants(pattern, src, found);
    }
}

/// The expression of the argument named `pattern`, or of the positional
/// argument at `position`.
fn pattern_argument<'t>(call: Node<'t>, position: usize, src: &[u8]) -> Option<Node<'t>> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let all: Vec<Node> = arguments
        .named_children(&mut cursor)
        .filter(|a| a.kind() == "argument")
        .collect();
    let name = |a: &Node| a.child_by_field_name("name").map(|n| text(n, src));
    let argument = all
        .iter()
        .find(|a| name(a) == Some("pattern"))
        .or_else(|| all.iter().filter(|a| name(a).is_none()).nth(position))?;
    argument.named_child(argument.named_child_count().checked_sub(1)?)
}

fn regex_mutants<'t>(literal: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let plain = matches!(
        literal.kind(),
        "string_literal" | "verbatim_string_literal" | "raw_string_literal"
    );
    let original = text(literal, src);
    let Some(value) = super::literal::csharp_value(original).filter(|_| plain) else {
        return;
    };
    for m in super::pattern::mutations(&value, super::pattern::Flavor::DotNet) {
        let replacement = super::literal::csharp_literal(&m.pattern, original);
        found.push(Found::new("regex", literal, replacement));
    }
}

fn interpolated<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let mut cursor = node.walk();
    let has_content = node.named_children(&mut cursor).any(|c| {
        c.kind().contains("content") || matches!(c.kind(), "interpolation" | "escape_sequence")
    });
    if has_content {
        found.push(Found::new("string", node, "$\"\""));
    }
}

fn string_empty<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    if is_string_type(node, "Empty", src) {
        found.push(Found::new("string", node, "\"dgf was here!\""));
    }
}

/// `string.<name>`, written with the `string` keyword.
fn is_string_type(node: Node, name: &str, src: &[u8]) -> bool {
    node.kind() == "member_access_expression"
        && node
            .child_by_field_name("expression")
            .is_some_and(|e| e.kind() == "predefined_type" && text(e, src) == "string")
        && node
            .child_by_field_name("name")
            .is_some_and(|n| text(n, src).starts_with(name))
}

/// LINQ method swaps.
const LINQ: &[(&str, &str)] = &[
    ("FirstOrDefault", "First"),
    ("First", "FirstOrDefault"),
    ("SingleOrDefault", "Single"),
    ("Single", "SingleOrDefault"),
    ("Last", "First"),
    ("All", "Any"),
    ("Any", "All"),
    ("Skip", "Take"),
    ("Take", "Skip"),
    ("SkipWhile", "TakeWhile"),
    ("TakeWhile", "SkipWhile"),
    ("Min", "Max"),
    ("Max", "Min"),
    ("Sum", "Max"),
    ("Count", "Sum"),
    ("Average", "Min"),
    ("OrderBy", "OrderByDescending"),
    ("OrderByDescending", "OrderBy"),
    ("ThenBy", "ThenByDescending"),
    ("ThenByDescending", "ThenBy"),
    ("Reverse", "AsEnumerable"),
    ("AsEnumerable", "Reverse"),
    ("Union", "Intersect"),
    ("Intersect", "Union"),
    ("Concat", "Except"),
    ("Except", "Concat"),
    ("MinBy", "MaxBy"),
    ("MaxBy", "MinBy"),
    ("SkipLast", "TakeLast"),
    ("TakeLast", "SkipLast"),
    ("Order", "OrderDescending"),
    ("OrderDescending", "Order"),
    ("UnionBy", "IntersectBy"),
    ("IntersectBy", "UnionBy"),
    ("Append", "Prepend"),
    ("Prepend", "Append"),
];

/// LINQ methods that need an argument: a call without one is not swapped to them.
const NEEDS_ARGUMENTS: &[&str] = &[
    "All",
    "SkipWhile",
    "TakeWhile",
    "OrderBy",
    "OrderByDescending",
    "ThenBy",
    "ThenByDescending",
    "Union",
    "Intersect",
    "SkipLast",
    "TakeLast",
    "MaxBy",
    "MinBy",
    "IntersectBy",
    "UnionBy",
    "Append",
    "Prepend",
];

/// `System.Math` swaps (Advanced).
const MATH: &[(&str, &[&str])] = &[
    ("Acos", &["Acosh", "Asin", "Atan"]),
    ("Acosh", &["Acos", "Asinh", "Atanh"]),
    ("Asin", &["Asinh", "Acos", "Atan"]),
    ("Asinh", &["Asin", "Acosh", "Atanh"]),
    ("Atan", &["Atanh", "Acos", "Asin"]),
    ("Atanh", &["Atan", "Acosh", "Asinh"]),
    ("BitDecrement", &["BitIncrement"]),
    ("BitIncrement", &["BitDecrement"]),
    ("Ceiling", &["Floor"]),
    ("Floor", &["Ceiling"]),
    ("Cos", &["Cosh", "Sin", "Tan"]),
    ("Cosh", &["Cos", "Sinh", "Tanh"]),
    ("Sin", &["Sinh", "Cos", "Tan"]),
    ("Sinh", &["Sin", "Cosh", "Tanh"]),
    ("Tan", &["Tanh", "Cos", "Sin"]),
    ("Tanh", &["Tan", "Cosh", "Sinh"]),
    ("Exp", &["Log"]),
    ("Log", &["Exp", "Pow"]),
    ("Pow", &["Log"]),
    ("MaxMagnitude", &["MinMagnitude"]),
    ("MinMagnitude", &["MaxMagnitude"]),
    ("ReciprocalEstimate", &["ReciprocalSqrtEstimate"]),
    ("ReciprocalSqrtEstimate", &["ReciprocalEstimate", "Sqrt"]),
];

/// String method swaps (Advanced).
const STRING_METHODS: &[(&str, &str)] = &[
    ("EndsWith", "StartsWith"),
    ("StartsWith", "EndsWith"),
    ("TrimStart", "TrimEnd"),
    ("TrimEnd", "TrimStart"),
    ("ToUpper", "ToLower"),
    ("ToLower", "ToUpper"),
    ("ToUpperInvariant", "ToLowerInvariant"),
    ("ToLowerInvariant", "ToUpperInvariant"),
    ("PadLeft", "PadRight"),
    ("PadRight", "PadLeft"),
    ("IndexOf", "LastIndexOf"),
    ("LastIndexOf", "IndexOf"),
];

/// A call and its method name.
struct Call<'t, 's> {
    node: Node<'t>,
    function: Node<'t>,
    name: Node<'t>,
    method: &'s str,
}

fn invocation<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    if is_string_type(function, "IsNullOr", src) {
        null_or_empty(node, function, src, found);
        return;
    }
    let Some(name) = method_name(function) else {
        return;
    };
    let call = Call {
        node,
        function,
        name,
        method: text(name, src),
    };
    linq(&call, found);
    math(&call, src, found);
    string_methods(&call, found);
}

/// `string.IsNullOrEmpty(x)` → `(x != null)`, `(x != "")`; WhiteSpace adds
/// `(x.Trim() != "")`.
fn null_or_empty<'t>(node: Node<'t>, function: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(first) = node
        .child_by_field_name("arguments")
        .and_then(|a| a.named_child(0))
    else {
        return;
    };
    let value = text(first, src);
    found.push(Found::new("string", node, format!("({value} != null)")));
    found.push(Found::new("string", node, format!("({value} != \"\")")));
    if text(function, src).ends_with("WhiteSpace") {
        found.push(Found::new(
            "string",
            node,
            format!("({value}.Trim() != \"\")"),
        ));
    }
}

fn linq<'t>(call: &Call<'t, '_>, found: &mut Vec<Found<'t>>) {
    let Some((_, to)) = LINQ.iter().find(|(from, _)| *from == call.method) else {
        return;
    };
    let arguments = call.node.child_by_field_name("arguments");
    // `Count(x => x.Done)` -> `Sum(x => x.Done)` asks Sum to add up booleans:
    // it never compiles.
    let predicate = arguments.and_then(|a| a.named_child(0)).is_some_and(|a| {
        a.named_child(0)
            .is_some_and(|l| l.kind() == "lambda_expression")
    });
    if call.method == "Count" && predicate {
        return;
    }
    let arguments = arguments.map_or(0, |a| a.named_child_count());
    if arguments > 0 || !NEEDS_ARGUMENTS.contains(to) {
        found.push(Found::part("linq", call.node, call.name, *to));
    }
}

fn math<'t>(call: &Call<'t, '_>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let receiver = call
        .function
        .child_by_field_name("expression")
        .map(|r| text(r, src));
    let swaps = MATH
        .iter()
        .find(|(from, _)| *from == call.method)
        .filter(|_| matches!(receiver, Some("Math" | "System.Math")));
    for replacement in swaps.map_or(&[][..], |(_, to)| *to) {
        found.push(Found::part("math", call.node, call.name, *replacement));
    }
}

fn string_methods<'t>(call: &Call<'t, '_>, found: &mut Vec<Found<'t>>) {
    if let Some((_, to)) = STRING_METHODS.iter().find(|(from, _)| *from == call.method) {
        found.push(Found::part("stringmethod", call.node, call.name, *to));
    }
    if matches!(call.method, "Trim" | "Substring") {
        found.push(Found::new("stringmethod", call.node, "\"\""));
    }
}

/// The method name of `x.Name(...)`, `x?.Name(...)` or `x.Name<T>(...)`.
fn method_name(function: Node) -> Option<Node> {
    let access = match function.kind() {
        "member_access_expression" => function,
        "conditional_access_expression" => {
            let mut cursor = function.walk();
            let last = function.named_children(&mut cursor).last()?;
            (last.kind() == "member_binding_expression").then_some(last)?
        }
        _ => return None,
    };
    let name = access.child_by_field_name("name")?;
    match name.kind() {
        "identifier" => Some(name),
        "generic_name" => name.named_child(0),
        _ => None,
    }
}

/// `new T { ... }`, `new int[] { ... }` and `stackalloc int[] { ... }` lose
/// their elements.
fn initializer<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let mut cursor = node.walk();
    let initializer = node.child_by_field_name("initializer").or_else(|| {
        node.named_children(&mut cursor)
            .find(|c| c.kind() == "initializer_expression")
    });
    if let Some(initializer) = initializer.filter(|i| i.named_child_count() > 0) {
        found.push(Found::part("initializer", node, initializer, "{ }"));
    }
}

/// `int[] a = { 1, 2 }` becomes `int[] a = { }`. A bare `{ ... }` is no
/// expression, so both arms of the switch get the declared type in front:
/// `(on ? new int[] { } : new int[] { 1, 2 })`.
fn array_initializer<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let declared = node
        .parent()
        .filter(|p| p.kind() == "variable_declarator" && node.named_child_count() > 0)
        .and_then(|p| p.parent())
        .and_then(|declaration| declaration.child_by_field_name("type"));
    let array = declared.map(|t| match t.kind() {
        "nullable_type" => t.named_child(0).unwrap_or(t),
        _ => t,
    });
    if let Some(array) = array.filter(|t| t.kind() == "array_type") {
        found.push(Found {
            prefix: Some(format!("new {}", text(array, src))),
            ..Found::new("initializer", node, "{ }")
        });
    }
}

fn collection<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let replacement = if node.named_child_count() == 0 {
        "[default]"
    } else {
        "[]"
    };
    found.push(Found::new("collection", node, replacement));
}

/// `x is P` becomes `x is not P` and back. A pattern that declares a
/// variable is left alone: the variable would be unassigned where it is used.
fn is_pattern<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return;
    };
    if !declared(pattern, src).is_empty() {
        return;
    }
    let negated = pattern.kind() == "negated_pattern";
    let Some(inner) = (if negated {
        pattern.named_child(0)
    } else {
        Some(pattern)
    }) else {
        return;
    };
    if let Some(constant) = misparsed_constant(inner) {
        found.push(misread_is(node, pattern, constant, negated, src));
    } else {
        let replacement = flipped_pattern(pattern, inner, negated, src);
        found.push(Found::part("equality", node, pattern, replacement));
    }
}

/// `x is null || y` read as `x is (null || y)`: only `x is null` is the
/// pattern, and only it is switched.
fn misread_is<'t>(
    node: Node<'t>,
    pattern: Node<'t>,
    constant: Node<'t>,
    negated: bool,
    src: &[u8],
) -> Found<'t> {
    let (start, replacement) = if negated {
        (pattern.start_byte(), text(constant, src).to_string())
    } else {
        (
            constant.start_byte(),
            format!("not {}", text(constant, src)),
        )
    };
    Found {
        mutator: "equality",
        node,
        start,
        end: constant.end_byte(),
        replacement,
        site: Site {
            end: constant.end_byte(),
            ..Site::of(node)
        },
        site_node: node,
        prefix: None,
    }
}

/// `not P`, or `P` for `not P`; `and`/`or` patterns get parentheses, so the
/// text means what the tree does.
fn flipped_pattern(pattern: Node, inner: Node, negated: bool, src: &[u8]) -> String {
    if negated {
        text(inner, src).to_string()
    } else if matches!(pattern.kind(), "and_pattern" | "or_pattern") {
        format!("not ({})", text(pattern, src))
    } else {
        format!("not {}", text(pattern, src))
    }
}

const RELATIONAL_PATTERNS: &[(&str, &[&str])] = &[
    ("<", &[">", "<="]),
    ("<=", &[">", "<"]),
    (">", &["<", ">="]),
    (">=", &["<", ">"]),
];

fn relational_pattern<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    let Some(op) = node.child(0) else {
        return;
    };
    let Some((_, replacements)) = RELATIONAL_PATTERNS.iter().find(|(o, _)| *o == op.kind()) else {
        return;
    };
    for replacement in *replacements {
        found.push(pattern_part("relational", node, op, replacement));
    }
}

fn binary_pattern<'t>(node: Node<'t>, _src: &[u8], found: &mut Vec<Found<'t>>) {
    if let Some(op) = node.child_by_field_name("operator") {
        let flipped = if op.kind() == "and" { "or" } else { "and" };
        found.push(pattern_part("logical", node, op, flipped));
    }
}

/// A change inside a pattern: patterns take no switch, so it goes on the
/// `is` expression or `switch` around the pattern.
fn pattern_part<'t>(
    mutator: &'static str,
    node: Node<'t>,
    part: Node<'t>,
    replacement: &str,
) -> Found<'t> {
    let mut found = Found::part(mutator, node, part, replacement);
    let mut current = node.parent();
    while let Some(parent) = current {
        let site = match parent.kind() {
            "is_pattern_expression" | "switch_expression" => Site::of(parent),
            "switch_statement" => Site::statement(parent),
            _ => {
                current = parent.parent();
                continue;
            }
        };
        found.site = site;
        found.site_node = parent;
        return found;
    }
    found
}

/// Block removal (Basic): a block becomes `{}`, or `{return default;}` where
/// the code after it would otherwise need a value.
fn block<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    if !has_statements(node) || infinite_loop_body(node, src) || block_cannot_change(node) {
        return;
    }
    let function = enclosing_function(node);
    let replacement = block_replacement(node, function, src);
    if unassigns(node, function, replacement, src) {
        return;
    }
    found.push(Found::new("block", node, replacement).switched_as_statement());
}

/// Whether `replacement` in place of the block `node` can leave a variable
/// unassigned: an `out` parameter of the function, or, for `{}`, a local
/// guarded by the block's jump or assigned in it for later.
fn unassigns(node: Node, function: Option<Node>, replacement: &str, src: &[u8]) -> bool {
    if function.is_some_and(|f| has_out_parameter(f, src)) {
        return true;
    }
    replacement == "{}"
        && ((has_kind_outside_functions_any(node, JUMPS) && guards_assignment(node, src))
            || assigns_for_later(node, src))
}

fn has_statements(node: Node) -> bool {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .any(|c| !c.kind().contains("comment"));
    found
}

fn infinite_loop_body(node: Node, src: &[u8]) -> bool {
    node.parent().is_some_and(|p| {
        p.kind() == "while_statement"
            && p.child_by_field_name("condition")
                .is_some_and(|c| text(c, src) == "true")
    })
}

fn block_replacement(node: Node, function: Option<Node>, src: &[u8]) -> &'static str {
    let is_body = function.is_some_and(|f| f.child_by_field_name("body") == Some(node));
    if function.is_some_and(is_iterator) {
        return iterator_block(is_body);
    }
    let needs_value =
        returns_value_inside(node) || (is_body && function.is_some_and(|f| returns_value(f, src)));
    if needs_value {
        "{return default;}"
    } else {
        "{}"
    }
}

/// An iterator's body must still be an iterator; inner blocks just empty.
fn iterator_block(is_body: bool) -> &'static str {
    if is_body {
        "{yield break;}"
    } else {
        "{}"
    }
}

/// Blocks that are not removed: blocks in a switch section, and blocks of struct
/// constructors that assign.
fn block_cannot_change(node: Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "local_function_statement" => return false,
            "switch_section" => return true,
            "constructor_declaration" if in_struct(parent) => return assigns(node),
            _ => current = parent.parent(),
        }
    }
    false
}

fn in_struct(constructor: Node) -> bool {
    constructor
        .parent()
        .and_then(|p| p.parent())
        .is_some_and(|t| t.kind().starts_with("struct") || t.kind().starts_with("record_struct"))
}

fn assigns(block: Node) -> bool {
    let mut cursor = block.walk();
    let found = block.named_children(&mut cursor).any(|c| {
        c.kind() == "expression_statement"
            && c.named_child(0)
                .is_some_and(|e| e.kind() == "assignment_expression")
    });
    found
}

const FUNCTIONS: &[&str] = &[
    "method_declaration",
    "local_function_statement",
    "lambda_expression",
    "anonymous_method_expression",
    "accessor_declaration",
    "constructor_declaration",
    "destructor_declaration",
    "operator_declaration",
    "conversion_operator_declaration",
    "property_declaration",
    "indexer_declaration",
];

/// Statement removal (Standard): the statement becomes `;`.
fn statement<'t>(node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>) {
    if kept_statement(node, src) || pointless_or_broken(node, src) {
        return;
    }
    if JUMPS.contains(&node.kind()) && guards_assignment(node, src) {
        return;
    }
    found.push(Found::new("statement", node, ";").switched_as_statement());
}

/// Statements that are not removed: `return x;`, `return;` in an `if`
/// that declares a pattern variable, jumps in a `switch` section, and
/// expression statements that assign or pass `out`.
fn kept_statement(node: Node, src: &[u8]) -> bool {
    match node.kind() {
        "return_statement" => keeps_return(node, src),
        "expression_statement" => {
            has_kind_outside_functions(node, "assignment_expression") || has_out_argument(node)
        }
        "yield_statement" => false,
        _ => has_ancestor(node, "switch_section"),
    }
}

fn keeps_return(node: Node, src: &[u8]) -> bool {
    node.named_child_count() > 0
        || in_declaring_if(node, src)
        || has_ancestor(node, "switch_section")
}

/// Removals that change nothing (the last `return;` or `continue;`) or
/// cannot compile (the last `throw` of a function that returns a value).
fn pointless_or_broken(node: Node, src: &[u8]) -> bool {
    let Some(function) = is_last_in_body(node) else {
        return false;
    };
    match node.kind() {
        "return_statement" | "continue_statement" => true,
        "throw_statement" => function.kind() == "lambda_expression" || returns_value(function, src),
        _ => false,
    }
}

const JUMPS: &[&str] = &[
    "return_statement",
    "throw_statement",
    "break_statement",
    "continue_statement",
    "goto_statement",
];

fn has_kind_outside_functions_any(node: Node, kinds: &[&str]) -> bool {
    let mut found = false;
    visit_outside_functions(node, &mut |n| found |= kinds.contains(&n.kind()));
    found
}

/// Whether removing a jump here would let later code run without a
/// variable it relies on, so the mutated program is not valid C#:
/// `if (!map.TryGetValue(k, out var v)) { return; } use(v);`, or
/// `int x; try { x = Read(); } catch { return; } use(x);`.
fn guards_assignment(node: Node, src: &[u8]) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "if_statement" if parent.child_by_field_name("condition") != Some(current) => {
                let names = parent
                    .child_by_field_name("condition")
                    .map(|c| declared(c, src))
                    .unwrap_or_default();
                return !names.is_empty() && used_after(parent, &names, src);
            }
            "catch_clause" => return try_assigns_for_later(parent, src),
            kind if FUNCTIONS.contains(&kind) => return false,
            _ => current = parent,
        }
    }
    false
}

/// Whether the `try` of `catch` assigns a variable that is declared before
/// it without a value and used after it, or an `out` parameter.
fn try_assigns_for_later(catch: Node, src: &[u8]) -> bool {
    let Some(tried) = catch.parent().filter(|t| t.kind() == "try_statement") else {
        return false;
    };
    let Some(body) = tried.child_by_field_name("body") else {
        return false;
    };
    let mut assigned = Vec::new();
    visit_outside_functions(body, &mut |n| {
        let left = n
            .child_by_field_name("left")
            .filter(|l| n.kind() == "assignment_expression" && l.kind() == "identifier");
        assigned.extend(left.map(|l| text(l, src).to_string()));
    });
    let outs = enclosing_function(catch)
        .map(|f| out_parameters(f, src))
        .unwrap_or_default();
    let unset = unset_before(tried, src);
    assigned.iter().any(|a| outs.contains(a))
        || used_after(
            tried,
            &assigned
                .into_iter()
                .filter(|a| unset.contains(a))
                .collect::<Vec<_>>(),
            src,
        )
}

/// Whether `block` assigns a local that is declared without a value outside
/// it and used after it: `int x; if (c) { x = 1; } else { x = 2; } use(x);`.
fn assigns_for_later(block: Node, src: &[u8]) -> bool {
    let assigned = assigned_names(block, src);
    if assigned.is_empty() {
        return false;
    }
    let mut current = block;
    while let Some(parent) = current.parent() {
        if FUNCTIONS.contains(&parent.kind()) {
            return false;
        }
        if parent.kind() == "block" && unset_and_used_after(current, &assigned, src) {
            return true;
        }
        current = parent;
    }
    false
}

/// The plain names `node` assigns, outside nested functions.
fn assigned_names(node: Node, src: &[u8]) -> Vec<String> {
    let mut assigned = Vec::new();
    visit_outside_functions(node, &mut |n| {
        let left = n
            .child_by_field_name("left")
            .filter(|l| n.kind() == "assignment_expression" && l.kind() == "identifier");
        assigned.extend(left.map(|l| text(l, src).to_string()));
    });
    assigned
}

/// Whether some of `names` are declared without a value before `statement`
/// and used after it.
fn unset_and_used_after(statement: Node, names: &[String], src: &[u8]) -> bool {
    let unset = unset_before(statement, src);
    let declared_here: Vec<String> = names
        .iter()
        .filter(|a| unset.contains(a))
        .cloned()
        .collect();
    !declared_here.is_empty() && used_after(statement, &declared_here, src)
}

/// Locals declared without a value before `statement` in its block.
fn unset_before(statement: Node, src: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut sibling = statement.prev_named_sibling();
    while let Some(previous) = sibling {
        if previous.kind() == "local_declaration_statement" {
            super::walk(previous, &mut |n| {
                if n.kind() == "variable_declarator" && declarator_value(n).is_none() {
                    names.extend(name_of(n, src));
                }
            });
        }
        sibling = previous.prev_named_sibling();
    }
    names
}

fn out_parameters(function: Node, src: &[u8]) -> Vec<String> {
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut cursor = parameters.walk();
    let names = parameters
        .named_children(&mut cursor)
        .filter(|p| {
            let mut inner = p.walk();
            let out = p.children(&mut inner).any(|c| text(c, src) == "out");
            out
        })
        .filter_map(|p| name_of(p, src))
        .collect();
    names
}

/// `return;` inside an `if` that declares a pattern variable.
fn in_declaring_if(node: Node, src: &[u8]) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "if_statement" {
            return parent
                .child_by_field_name("condition")
                .is_some_and(|c| !declared(c, src).is_empty());
        }
        current = parent.parent();
    }
    false
}

/// The function `node` ends (control flows from it to the function's end),
/// if any: it is last in its block, and that block is the body, or last in
/// an `if`/`else` that is itself last.
fn is_last_in_body(node: Node) -> Option<Node> {
    let mut current = node;
    loop {
        let parent = current.parent()?;
        match parent.kind() {
            "block" => {
                if last_statement(parent) != Some(current) {
                    return None;
                }
                let owner = parent.parent()?;
                if FUNCTIONS.contains(&owner.kind()) {
                    return Some(owner);
                }
                current = parent;
            }
            "if_statement" | "else_clause" => current = parent,
            _ => return None,
        }
    }
}

fn last_statement(block: Node) -> Option<Node> {
    let mut cursor = block.walk();
    let last = block
        .named_children(&mut cursor)
        .filter(|c| !c.kind().contains("comment"))
        .last();
    last
}

fn enclosing_function(node: Node) -> Option<Node> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if FUNCTIONS.contains(&parent.kind()) {
            return Some(parent);
        }
        current = parent.parent();
    }
    None
}

/// Whether the function returns a value (`return default;` compiles in it).
fn returns_value(function: Node, src: &[u8]) -> bool {
    match function.kind() {
        "method_declaration" | "local_function_statement" => declared_value(function, src),
        "accessor_declaration" => function
            .child_by_field_name("name")
            .is_some_and(|n| text(n, src) == "get"),
        "operator_declaration"
        | "conversion_operator_declaration"
        | "property_declaration"
        | "indexer_declaration" => true,
        "lambda_expression" | "anonymous_method_expression" => returns_value_inside(function),
        _ => false,
    }
}

/// A declared return type other than `void` and an `async` method's `Task`.
fn declared_value(function: Node, src: &[u8]) -> bool {
    let Some(returns) = function
        .child_by_field_name("returns")
        .or_else(|| function.child_by_field_name("type"))
    else {
        return false;
    };
    let returns = text(returns, src);
    returns != "void" && !(is_async(function, src) && matches!(returns, "Task" | "ValueTask"))
}

fn is_async(function: Node, src: &[u8]) -> bool {
    let mut cursor = function.walk();
    let found = function
        .children(&mut cursor)
        .any(|c| text(c, src) == "async");
    found
}

/// A `return x;` in `node`, outside nested functions.
fn returns_value_inside(node: Node) -> bool {
    let mut found = false;
    visit_outside_functions(node, &mut |n| {
        found |= n.kind() == "return_statement" && n.named_child_count() > 0;
    });
    found
}

fn is_iterator(function: Node) -> bool {
    let Some(body) = function.child_by_field_name("body") else {
        return false;
    };
    let mut found = false;
    visit_outside_functions(body, &mut |n| found |= n.kind() == "yield_statement");
    found
}

fn has_out_parameter(function: Node, src: &[u8]) -> bool {
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return false;
    };
    let mut cursor = parameters.walk();
    let found = parameters.named_children(&mut cursor).any(|p| {
        let mut inner = p.walk();
        let out = p.children(&mut inner).any(|c| text(c, src) == "out");
        out
    });
    found
}

/// Calls `visit` on `node` and its descendants, without entering lambdas
/// or local functions.
fn visit_outside_functions(node: Node, visit: &mut dyn FnMut(Node)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !matches!(
            child.kind(),
            "lambda_expression" | "anonymous_method_expression" | "local_function_statement"
        ) {
            visit_outside_functions(child, visit);
        }
    }
}

fn has_kind_outside_functions(node: Node, kind: &str) -> bool {
    let mut found = false;
    visit_outside_functions(node, &mut |n| found |= n.kind() == kind);
    found
}

fn has_out_argument(node: Node) -> bool {
    let mut found = false;
    visit_outside_functions(node, &mut |n| found |= is_out_argument(n));
    found
}

fn is_out_argument(n: Node) -> bool {
    n.kind() == "argument" && n.child(0).is_some_and(|c| c.kind() == "out")
}

fn has_ancestor(node: Node, kind: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == kind {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn has_kind(node: Node, kinds: &[&str]) -> bool {
    let mut found = false;
    super::walk(node, &mut |n| found |= kinds.contains(&n.kind()));
    found
}

// ---------------------------------------------------------------------------
// The C# grammar reads `x is null || y > 1` as `x is (null || y > 1)`. The
// operators in such a "pattern" are real expressions that start at `x`.

/// For a binary expression on the left edge of a misread pattern, the `is`
/// expression it really belongs to.
fn misparsed_spine(node: Node) -> Option<Node> {
    let mut current = node;
    loop {
        let parent = current.parent()?;
        match parent.kind() {
            "binary_expression" if parent.child_by_field_name("left") == Some(current) => {
                current = parent;
            }
            "constant_pattern" | "relational_pattern" => return pattern_owner(parent),
            _ => return None,
        }
    }
}

/// The `is` expression whose pattern is `holder` (or `not holder`).
fn pattern_owner(holder: Node) -> Option<Node> {
    let pattern = match holder.parent()?.kind() {
        "negated_pattern" => holder.parent()?,
        _ => holder,
    };
    let is = pattern.parent()?;
    (is.kind() == "is_pattern_expression" && is.child_by_field_name("pattern") == Some(pattern))
        .then_some(is)
}

/// For a misread `is` expression, the binary expression the grammar put in
/// its pattern.
fn misparsed_root(is: Node) -> Option<Node> {
    let pattern = is.child_by_field_name("pattern")?;
    let holder = if pattern.kind() == "negated_pattern" {
        pattern.named_child(0)?
    } else {
        pattern
    };
    if !is_misread_holder(holder) {
        return None;
    }
    let root = holder.named_child(0)?;
    (root.kind() == "binary_expression").then_some(root)
}

/// For a misread pattern, its real constant: the leftmost operand.
fn misparsed_constant(pattern: Node) -> Option<Node> {
    if !is_misread_holder(pattern) {
        return None;
    }
    let mut current = pattern.named_child(0)?;
    if current.kind() != "binary_expression" {
        return None;
    }
    while current.kind() == "binary_expression" {
        current = current.child_by_field_name("left")?;
    }
    Some(current)
}

fn is_misread_holder(node: Node) -> bool {
    matches!(node.kind(), "constant_pattern" | "relational_pattern")
}

// ---------------------------------------------------------------------------
// Placement.

/// Moves an expression switch to where it can go: past the end of a `?.`
/// chain; around the whole statement for a whole expression statement or
/// `for` clause; nowhere for a whole lambda body that assigns, or a whole
/// `void` member body.
fn expression_site<'t>(
    found: &Found<'t>,
    node: Node<'t>,
    site: Site,
    src: &[u8],
) -> Option<(Node<'t>, Site)> {
    let (node, site) = past_chain(node, site)?;
    let Some(parent) = node.parent().filter(|_| is_whole(node, site)) else {
        return Some((node, site));
    };
    match parent.kind() {
        "expression_statement" => Some((parent, Site::statement(parent))),
        "for_statement" if is_for_clause(parent, node) => Some((parent, Site::statement(parent))),
        "lambda_expression" if matches!(found.mutator, "update" | "assignment") => None,
        "arrow_expression_clause" if is_void_member(parent, src) => None,
        _ => Some((node, site)),
    }
}

/// A switch in the middle of a `?.` chain would end the chain early and
/// turn its null check into a crash: it goes after the whole chain.
fn past_chain(mut node: Node, mut site: Site) -> Option<(Node, Site)> {
    while continues_chain(node) {
        node = node.parent()?;
        site = Site::of(node);
    }
    Some((node, site))
}

fn is_whole(node: Node, site: Site) -> bool {
    site.start == node.start_byte() && site.end == node.end_byte()
}

fn is_for_clause(for_statement: Node, node: Node) -> bool {
    for_statement.child_by_field_name("body") != Some(node)
        && for_statement.child_by_field_name("condition") != Some(node)
}

/// Whether the `=> ...` body belongs to a member that returns nothing.
fn is_void_member(arrow: Node, src: &[u8]) -> bool {
    let Some(owner) = arrow.parent() else {
        return false;
    };
    match owner.kind() {
        "constructor_declaration" | "destructor_declaration" => true,
        "accessor_declaration" => owner
            .child_by_field_name("name")
            .is_some_and(|n| text(n, src) != "get"),
        "method_declaration" | "local_function_statement" => owner
            .child_by_field_name("returns")
            .or_else(|| owner.child_by_field_name("type"))
            .is_some_and(|t| text(t, src) == "void"),
        _ => false,
    }
}

/// Whether `node` is followed by more of a `?.` chain.
fn continues_chain(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let continued = match parent.kind() {
        "member_access_expression" | "element_access_expression" => {
            parent.child_by_field_name("expression") == Some(node)
        }
        "invocation_expression" => parent.child_by_field_name("function") == Some(node),
        _ => false,
    };
    continued && has_conditional_access(node)
}

/// A `?.` on the left edge of `node`.
fn has_conditional_access(node: Node) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        current = match n.kind() {
            "conditional_access_expression" => return true,
            "member_access_expression" | "element_access_expression" => {
                n.child_by_field_name("expression")
            }
            "invocation_expression" => n.child_by_field_name("function"),
            _ => None,
        };
    }
    false
}

/// The last checks on a placed site: a statement switch needs a place for
/// a statement, and constants take no switch. A copy that holds a string
/// spanning lines or a preprocessor line keeps its line breaks, which an
/// interpolated string's `{...}` cannot take.
fn placed(node: Node, site: Site, found: &Found, src: &[u8]) -> Option<Site> {
    if (site.statement && !wrappable(node, src)) || needs_constant(node, src) {
        return None;
    }
    let multiline = keeps_lines(node, site, found, src);
    if multiline && has_ancestor(node, "interpolation") {
        return None;
    }
    Some(Site { multiline, ..site })
}

/// Whether the copy must keep its line breaks: a string spanning lines or a
/// preprocessor line in it, or a replacement that copied lines.
fn keeps_lines(node: Node, site: Site, found: &Found, src: &[u8]) -> bool {
    !fits_one_line(node, site, found)
        || has_directive_line(site, found, src)
        || found.replacement.contains('\n')
}

/// A preprocessor line in the copied part of `site`. The parse may not see
/// it: `first_branches` blanks `#if` lines for the grammar.
fn has_directive_line(site: Site, found: &Found, src: &[u8]) -> bool {
    [(site.start, found.start), (found.end, site.end)]
        .into_iter()
        .any(|(start, end)| {
            src[start..end.max(start)]
                .split(|&b| b == b'\n')
                .skip(1)
                .any(|line| line.trim_ascii_start().first() == Some(&b'#'))
        })
}

/// Whether a statement can be wrapped in `{if (...) {...} else {...}}`. A
/// labeled statement cannot: its label would name the wrapping block, and
/// `break outer;` needs it on the loop.
fn wrappable(node: Node, src: &[u8]) -> bool {
    if node.kind() == "block" {
        return true;
    }
    let container = node.parent().is_some_and(|p| {
        matches!(
            p.kind(),
            "block"
                | "switch_section"
                | "if_statement"
                | "else_clause"
                | "while_statement"
                | "do_statement"
                | "for_statement"
                | "foreach_statement"
                | "using_statement"
                | "lock_statement"
                | "fixed_statement"
        )
    });
    container && !is_using_declaration(node, src) && node.kind() != "local_function_statement"
}

/// `using var x = ...;`: wrapping it would dispose `x` early.
fn is_using_declaration(node: Node, src: &[u8]) -> bool {
    let mut cursor = node.walk();
    let using = node.kind() == "local_declaration_statement"
        && node.children(&mut cursor).any(|c| text(c, src) == "using");
    using
}

/// A mutant that only changes a condition's value, in a condition that
/// declares `out` variables (`if (!map.TryGetValue(k, out var v)) return;`):
/// switched as `(c) && !on`, `(c) || on` or `(c) != on`, so the condition
/// and its declarations are evaluated once and every `out` variable is
/// assigned as before. A side effect of `c`, such as `TryGetValue` itself,
/// still happens.
fn condition_switch<'t>(found: &Found<'t>, src: &[u8]) -> Option<(Node<'t>, Site)> {
    let condition = condition_of(found)?;
    let target = match found.mutator {
        "conditional" => found.node.child_by_field_name("condition")?,
        _ => found.site_node,
    };
    if declared(target, src).is_empty() || !always_assigned(target) {
        return None;
    }
    let site = Site {
        condition: Some(condition),
        ..Site::of(target)
    };
    Some((target, site))
}

fn condition_of(found: &Found) -> Option<Condition> {
    match (found.mutator, found.replacement.as_str()) {
        ("condition", _) | ("conditional", "false") => Some(Condition::False),
        ("conditional", _) => Some(Condition::True),
        ("negate" | "negation", _) => Some(Condition::Negated),
        _ => None,
    }
}

/// Whether every variable `condition` declares is an `out` argument it
/// always passes: not on the right of `&&`, `||` or `??`, nor in one branch
/// of a `?:`. (A pattern variable is only assigned when the pattern holds.)
fn always_assigned(condition: Node) -> bool {
    let mut all = true;
    visit_outside_functions(condition, &mut |n| {
        let declares = matches!(
            n.kind(),
            "declaration_pattern" | "recursive_pattern" | "var_pattern"
        ) || (n.kind() == "declaration_expression"
            && !n.parent().is_some_and(is_out_argument));
        all &= !declares;
        if n.kind() == "declaration_expression" || out_identifier(n).is_some() {
            all &= reached_always(n, condition);
        }
    });
    all
}

/// Whether evaluating `top` always evaluates `node`.
fn reached_always(node: Node, top: Node) -> bool {
    let mut current = node;
    while current != top {
        let Some(parent) = current.parent() else {
            return false;
        };
        let conditional = match parent.kind() {
            "binary_expression" => {
                parent.child_by_field_name("right") == Some(current)
                    && parent
                        .child_by_field_name("operator")
                        .is_some_and(|op| matches!(op.kind(), "&&" | "||" | "??"))
            }
            "conditional_expression" => parent.child_by_field_name("condition") != Some(current),
            _ => false,
        };
        if conditional {
            return false;
        }
        current = parent;
    }
    true
}

/// Variables that a switch would declare twice, or leave unassigned, move
/// the switch to the statement or drop the mutant.
fn with_declarations<'t>(
    found: &Found<'t>,
    node: Node<'t>,
    site: Site,
    src: &[u8],
) -> Option<(Node<'t>, Site)> {
    let names = declared_in_site(node, site, src);
    if names.is_empty() {
        return Some((node, site));
    }
    let statement = if site.statement {
        node
    } else {
        enclosing_statement(node)?
    };
    let mut names = scoped_names(statement, names, src);
    // Wrapping the statement hides everything it declares from the code
    // after it, not only what the mutant's own expression declares.
    names.extend(header_declarations(statement, src));
    if used_after(statement, &names, src) {
        return None;
    }
    if site.statement || !mutated_declares(found, node, &names, src) {
        return Some((node, site));
    }
    wrappable(statement, src).then_some((statement, Site::statement(statement)))
}

/// What a statement declares that the code after it can use: an `if`'s
/// condition, or the whole of an expression, return or declaration statement.
fn header_declarations(statement: Node, src: &[u8]) -> Vec<String> {
    match statement.kind() {
        "if_statement" => statement
            .child_by_field_name("condition")
            .map(|c| declared(c, src))
            .unwrap_or_default(),
        "expression_statement" | "return_statement" | "local_declaration_statement" => {
            declared(statement, src)
        }
        _ => Vec::new(),
    }
}

/// `names`, and the variables `statement` declares itself when it is a
/// declaration: wrapping it would hide them.
fn scoped_names(statement: Node, mut names: Vec<String>, src: &[u8]) -> Vec<String> {
    if statement.kind() == "local_declaration_statement" {
        names.extend(declared(statement, src));
    }
    names
}

/// Whether the mutated copy still declares one of `names`.
fn mutated_declares(found: &Found, node: Node, names: &[String], src: &[u8]) -> bool {
    let declarations = declarations(node, src);
    names.iter().any(|name| {
        found.replacement.contains(name.as_str())
            || declarations
                .iter()
                .any(|(at, n)| n == name && (*at < found.start || *at >= found.end))
    })
}

/// Names declared inside the site (pattern variables, `out var`) or
/// assigned by `out x`.
fn declared_in_site(node: Node, site: Site, src: &[u8]) -> Vec<String> {
    declarations(node, src)
        .into_iter()
        .filter(|(at, _)| site.start <= *at && *at < site.end)
        .map(|(_, name)| name)
        .collect()
}

fn declared(node: Node, src: &[u8]) -> Vec<String> {
    declarations(node, src)
        .into_iter()
        .map(|(_, n)| n)
        .collect()
}

/// Every variable `node` declares or assigns with `out`, with its position.
fn declarations(node: Node, src: &[u8]) -> Vec<(usize, String)> {
    let mut names = Vec::new();
    visit_outside_functions(node, &mut |n| {
        for id in declared_names(n) {
            names.push((id.start_byte(), text(id, src).to_string()));
        }
    });
    names
}

/// The identifiers `node` itself declares, or assigns with `out x`.
fn declared_names(node: Node) -> Vec<Node> {
    match node.kind() {
        "declaration_expression"
        | "declaration_pattern"
        | "recursive_pattern"
        | "var_pattern"
        | "variable_declarator" => node.child_by_field_name("name").into_iter().collect(),
        "argument" => out_identifier(node).into_iter().collect(),
        "parenthesized_variable_designation" => {
            let mut cursor = node.walk();
            let ids = node
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "identifier")
                .collect();
            ids
        }
        _ => Vec::new(),
    }
}

/// `x` in an `out x` argument.
fn out_identifier(argument: Node) -> Option<Node> {
    argument.child(0).filter(|c| c.kind() == "out")?;
    argument.named_child(0).filter(|e| e.kind() == "identifier")
}

fn enclosing_statement(node: Node) -> Option<Node> {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind().ends_with("_statement") {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// Whether code after `statement`, in the same scope, uses one of `names`.
fn used_after(statement: Node, names: &[String], src: &[u8]) -> bool {
    let Some(parent) = statement.parent() else {
        return false;
    };
    let mut following = siblings_after(statement);
    if parent.kind() == "switch_section" {
        following.extend(siblings_after(parent));
    } else if parent.kind() != "block" {
        return false;
    }
    following.iter().any(|n| uses(*n, names, src))
}

fn siblings_after(node: Node) -> Vec<Node> {
    let mut following = Vec::new();
    let mut sibling = node.next_named_sibling();
    while let Some(next) = sibling {
        following.push(next);
        sibling = next.next_named_sibling();
    }
    following
}

fn uses(node: Node, names: &[String], src: &[u8]) -> bool {
    let mut found = false;
    super::walk(node, &mut |n| {
        found |= n.kind() == "identifier" && names.iter().any(|name| name == text(n, src));
    });
    found
}

/// Places that only take compile-time constants: patterns, attributes,
/// parameter defaults, enum members, `const`. The misread `x is null || y`
/// is not one for the part after `||`.
fn needs_constant(node: Node, src: &[u8]) -> bool {
    let mut previous = node;
    let mut current = node.parent();
    let mut escaped = false;
    while let Some(parent) = current {
        escaped |= escapes_misread_pattern(parent, previous);
        if is_misread_holder(parent) && escaped {
            escaped = false;
        } else if is_constant_place(parent, src) {
            return true;
        }
        previous = parent;
        current = parent.parent();
    }
    false
}

fn escapes_misread_pattern(parent: Node, previous: Node) -> bool {
    parent.kind() == "binary_expression"
        && parent.child_by_field_name("right") == Some(previous)
        && misparsed_spine(parent).is_some()
}

fn is_constant_place(node: Node, src: &[u8]) -> bool {
    let kind = node.kind();
    (kind.contains("pattern") && kind != "is_pattern_expression")
        || matches!(
            kind,
            "parameter" | "attribute_argument" | "attribute_list" | "enum_member_declaration"
        )
        || is_const_declaration(node, src)
}

fn is_const_declaration(node: Node, src: &[u8]) -> bool {
    if !matches!(
        node.kind(),
        "field_declaration" | "local_declaration_statement"
    ) {
        return false;
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|child| child.kind() == "modifier" && text(child, src) == "const");
    found
}

/// Whether the mutated copy can go on one line: not when the site holds a
/// preprocessor line or a string spanning lines (outside the replaced part).
fn fits_one_line(node: Node, site: Site, found: &Found) -> bool {
    let mut fits = true;
    super::walk(node, &mut |n| fits &= !breaks_one_line(n, site, found));
    fits
}

fn breaks_one_line(node: Node, site: Site, found: &Found) -> bool {
    within(site.start, site.end, node)
        && !within(found.start, found.end, node)
        && (is_multiline_string(node) || node.kind().starts_with("preproc"))
}

fn within(start: usize, end: usize, node: Node) -> bool {
    start <= node.start_byte() && node.end_byte() <= end
}

fn is_multiline_string(node: Node) -> bool {
    (node.kind().contains("string_literal") || node.kind() == "interpolated_string_expression")
        && node.start_position().row != node.end_position().row
}

// ---------------------------------------------------------------------------
// Date boundaries. Without types, an operand is a date or time when the file
// declares it with such a type, or its name says so (`EndDate`, `LastDay`,
// `CreatedAt`, `ExpiresOn`, `Today`, `UtcNow`).

const DATE_TYPES: &[&str] = &["DateOnly", "DateTime", "DateTimeOffset", "TimeOnly"];

/// Endings of a name that holds a date or time, after a lower-case letter.
const DATE_ENDINGS: &[&str] = &["Date", "Day", "Time", "At", "On", "Today", "Now"];

/// Whether the comparison `node` (a binary expression or a relational
/// pattern) has a date or time on either side.
fn compares_dates(node: Node, src: &[u8]) -> bool {
    let root = std::iter::successors(Some(node), |n| n.parent())
        .last()
        .unwrap_or(node);
    let dates = date_names(root, src);
    compared(node)
        .into_iter()
        .any(|operand| is_date(operand, &dates, src))
}

/// Both sides of a comparison: the operands, or for `x is > d` the pattern's
/// value and `x`.
fn compared(node: Node) -> Vec<Node> {
    if node.kind() == "binary_expression" {
        return ["left", "right"]
            .iter()
            .filter_map(|f| node.child_by_field_name(f))
            .collect();
    }
    let subject = std::iter::successors(node.parent(), |n| n.parent())
        .find(|n| {
            matches!(
                n.kind(),
                "is_pattern_expression" | "switch_expression" | "switch_statement"
            )
        })
        .and_then(|owner| {
            owner
                .child_by_field_name("expression")
                .or_else(|| owner.child_by_field_name("value"))
                .or_else(|| owner.named_child(0))
        });
    node.named_child(0).into_iter().chain(subject).collect()
}

/// Names the file declares with a date or time type, or sets from one.
fn date_names(root: Node, src: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    super::walk(root, &mut |n| {
        let typed = n
            .child_by_field_name("type")
            .is_some_and(|t| is_date_type(text(t, src)));
        match n.kind() {
            "parameter" | "property_declaration" if typed => names.extend(name_of(n, src)),
            "variable_declaration" => {
                let mut cursor = n.walk();
                for declarator in n.named_children(&mut cursor) {
                    let set_from_date = declarator_value(declarator)
                        .is_some_and(|v| DATE_TYPES.iter().any(|t| text(v, src).contains(t)));
                    if typed || set_from_date {
                        names.extend(name_of(declarator, src));
                    }
                }
            }
            _ => {}
        }
    });
    names
}

fn is_date_type(text: &str) -> bool {
    DATE_TYPES.contains(&text.trim_end_matches('?'))
}

/// Whether an operand is a date or time: a declared name, a name that says
/// so, or an expression that builds one.
fn is_date(operand: Node, dates: &[String], src: &[u8]) -> bool {
    let name = last_name(operand, src);
    dates.contains(&name)
        || date_word(&name)
        || DATE_TYPES.iter().any(|t| text(operand, src).contains(t))
}

/// The name an operand ends with: `x`, `order.EndDate`, `clock.Today()`.
fn last_name(operand: Node, src: &[u8]) -> String {
    let named = match operand.kind() {
        "member_access_expression" | "member_binding_expression" => {
            operand.child_by_field_name("name")
        }
        "invocation_expression" => operand
            .child_by_field_name("function")
            .and_then(|f| f.child_by_field_name("name").or(Some(f))),
        "parenthesized_expression" => operand.named_child(0),
        _ => Some(operand),
    };
    named.map(|n| text(n, src).to_string()).unwrap_or_default()
}

fn date_word(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(lower.as_str(), "date" | "day" | "time" | "today" | "now")
        || DATE_ENDINGS.iter().any(|ending| {
            name.strip_suffix(ending)
                .and_then(|head| head.chars().last())
                .is_some_and(|c| c.is_lowercase())
        })
}

// ---------------------------------------------------------------------------
// ignore_methods.

/// What the ignored-call walk reads.
struct Calls<'a> {
    src: &'a [u8],
    methods: &'a MethodFilter,
}

impl Calls<'_> {
    fn constructor(&self, type_node: Node) -> bool {
        self.methods
            .matches(&format!("{}.ctor", compact(type_node, self.src)))
    }
}

/// Whether `node` is inside a call `ignore_methods` names; `up` lets the
/// walk go on to the parent.
fn in_ignored_call(node: Node, calls: &Calls, up: bool) -> bool {
    match decides(node, calls) {
        Some(answer) => answer,
        None => {
            up && node
                .parent()
                .is_some_and(|p| in_ignored_call(p, calls, true))
        }
    }
}

/// The answer at `node`, or `None` to ask its parent.
type Decider = fn(Node, &Calls) -> Option<bool>;

const DECIDERS: &[(&str, Decider)] = &[
    ("invocation_expression", ignored_invocation),
    ("object_creation_expression", ignored_creation),
    (
        "implicit_object_creation_expression",
        ignored_implicit_creation,
    ),
    ("conditional_access_expression", ignored_conditional_access),
    ("conditional_expression", ignored_conditional),
    ("expression_statement", ignored_first_child),
    ("await_expression", ignored_first_child),
    ("assignment_expression", ignored_assignment),
    ("local_declaration_statement", ignored_declaration),
    ("block", ignored_block),
    ("constructor_initializer", ignored_constructor_call),
];

fn decides(node: Node, calls: &Calls) -> Option<bool> {
    match DECIDERS.iter().find(|(kind, _)| *kind == node.kind()) {
        Some((_, decide)) => decide(node, calls),
        // The walk stops at a member: a method's own declaration is not a call.
        None => is_member(node.kind()).then_some(false),
    }
}

fn ignored_invocation(node: Node, calls: &Calls) -> Option<bool> {
    let name = node
        .child_by_field_name("function")
        .map(|f| compact(f, calls.src))
        .unwrap_or_default();
    (calls.methods.matches(&name) || chained_into_ignored(node, calls)).then_some(true)
}

/// `a.Ignored(x).Next()` and `x?.Ignored()`: the call `node` is chained into.
fn chained_into_ignored(node: Node, calls: &Calls) -> bool {
    let Some(parent) = node.parent().filter(|p| {
        matches!(
            p.kind(),
            "member_access_expression" | "conditional_access_expression"
        )
    }) else {
        return false;
    };
    parent.parent().is_some_and(|g| {
        g.kind() == "invocation_expression"
            && g.child_by_field_name("function") == Some(parent)
            && in_ignored_call(g, calls, false)
    })
}

fn ignored_creation(node: Node, calls: &Calls) -> Option<bool> {
    Some(
        node.child_by_field_name("type")
            .is_some_and(|t| calls.constructor(t)),
    )
}

/// `T x = new(...)`: the constructor of the declared type.
fn ignored_implicit_creation(node: Node, calls: &Calls) -> Option<bool> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "variable_declaration" {
            return Some(
                parent
                    .child_by_field_name("type")
                    .is_some_and(|t| calls.constructor(t)),
            );
        }
        current = parent.parent();
    }
    Some(false)
}

fn ignored_conditional_access(node: Node, calls: &Calls) -> Option<bool> {
    Some(node.parent().is_some_and(|p| {
        p.kind() == "invocation_expression"
            && p.child_by_field_name("function") == Some(node)
            && in_ignored_call(p, calls, false)
    }))
}

/// A `?:` whose both branches are ignored calls.
fn ignored_conditional(node: Node, calls: &Calls) -> Option<bool> {
    let branch = |field| {
        node.child_by_field_name(field)
            .is_some_and(|b| in_ignored_call(b, calls, false))
    };
    (branch("consequence") && branch("alternative")).then_some(true)
}

fn ignored_first_child(node: Node, calls: &Calls) -> Option<bool> {
    Some(
        node.named_child(0)
            .is_some_and(|e| in_ignored_call(e, calls, false)),
    )
}

fn ignored_assignment(node: Node, calls: &Calls) -> Option<bool> {
    Some(
        node.child_by_field_name("right")
            .is_some_and(|r| in_ignored_call(r, calls, false)),
    )
}

/// A declaration whose every value is an ignored call.
fn ignored_declaration(node: Node, calls: &Calls) -> Option<bool> {
    let mut values = Vec::new();
    super::walk(node, &mut |n| {
        if n.kind() == "variable_declarator" {
            values.push(declarator_value(n));
        }
    });
    let all = values
        .iter()
        .all(|v| v.is_some_and(|v| in_ignored_call(v, calls, false)));
    Some(!values.is_empty() && all)
}

/// A block of ignored calls only; an empty block asks its parent.
fn ignored_block(node: Node, calls: &Calls) -> Option<bool> {
    let mut cursor = node.walk();
    let statements: Vec<Node> = node
        .named_children(&mut cursor)
        .filter(|c| !c.kind().contains("comment"))
        .collect();
    if statements.is_empty() {
        return None;
    }
    Some(statements.iter().all(|s| in_ignored_call(*s, calls, false)))
}

fn ignored_constructor_call(node: Node, calls: &Calls) -> Option<bool> {
    Some(
        calls
            .methods
            .matches(&format!("{}.ctor", compact(node, calls.src))),
    )
}

fn is_member(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "property_declaration"
            | "indexer_declaration"
            | "field_declaration"
            | "event_declaration"
            | "event_field_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration"
            | "class_declaration"
            | "struct_declaration"
            | "record_declaration"
            | "record_struct_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "delegate_declaration"
            | "namespace_declaration"
            | "file_scoped_namespace_declaration"
    )
}

/// Inside a member (or type) marked `[ExcludeFromCodeCoverage]`.
fn excluded_from_coverage(node: Node, src: &[u8]) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        if is_member(n.kind()) && has_exclude_attribute(n, src) {
            return true;
        }
        current = n.parent();
    }
    false
}

fn has_exclude_attribute(member: Node, src: &[u8]) -> bool {
    let mut cursor = member.walk();
    let lists: Vec<Node> = member
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "attribute_list")
        .collect();
    lists.iter().any(|list| {
        let mut inner = list.walk();
        let any = list
            .named_children(&mut inner)
            .any(|a| is_exclude_attribute(a, src));
        any
    })
}

fn is_exclude_attribute(attribute: Node, src: &[u8]) -> bool {
    attribute.child_by_field_name("name").is_some_and(|name| {
        let name = compact(name, src);
        let name = name
            .strip_prefix("System.Diagnostics.CodeAnalysis.")
            .unwrap_or(&name);
        matches!(
            name,
            "ExcludeFromCodeCoverage" | "ExcludeFromCodeCoverageAttribute"
        )
    })
}
