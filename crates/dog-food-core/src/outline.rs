//! The declarations of a file without their bodies: namespaces, types and
//! their members, each with the lines it spans and its signature. Bodies
//! are not entered, so local functions and closures are not listed.

use tree_sitter::Node;

use crate::{Error, Language};

/// One declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// `class`, `method`, `field`, `fn`, ...
    pub kind: &'static str,
    pub name: String,
    /// The declaration as written, on one line, without its body.
    pub signature: String,
    /// 1-based, inclusive; attributes and decorators belong to the range.
    pub start_line: usize,
    pub end_line: usize,
    /// The first line of the comment right above, else `start_line`.
    pub doc_line: usize,
    /// Where the name is written, in bytes.
    pub name_byte: usize,
    /// How many declarations this one is inside.
    pub depth: usize,
    /// The declaration it is a member of: an index into the same list.
    pub parent: Option<usize>,
    /// A namespace or type, which holds other declarations.
    pub container: bool,
    /// Not visible outside its type, or its module when it is not in a type.
    pub private: bool,
}

/// The longest signature kept; a longer one is cut and ends in `...`.
const SIGNATURE_LIMIT: usize = 200;

/// Every declaration of `source`, in the order written.
pub fn outline(source: &str, language: Language) -> Result<Vec<Symbol>, Error> {
    crate::guarded("<memory>", || {
        let (tree, _) = crate::parse_prepared(source, language, "<memory>")?;
        Ok(declared(&tree, source, language))
    })
}

fn declared(tree: &tree_sitter::Tree, source: &str, language: Language) -> Vec<Symbol> {
    let mut walk = Walk {
        src: source.as_bytes(),
        language,
        rules: rules(language),
        out: Vec::new(),
    };
    let top = Place {
        depth: 0,
        parent: None,
        open: walk.rules.open_at_top,
    };
    walk.children(tree.root_node(), top, None);
    walk.out
}

/// [`outline`] and [`uses`] of one source, which is read once for both.
pub fn outline_and_uses(
    source: &str,
    language: Language,
    names: &[&str],
) -> Result<(Vec<Symbol>, Vec<Use>), Error> {
    crate::guarded("<memory>", || {
        let (tree, _) = crate::parse_prepared(source, language, "<memory>")?;
        let symbols = declared(&tree, source, language);
        Ok((symbols, written(&tree, source.as_bytes(), names)))
    })
}

/// The names from the outermost declaration to `symbols[index]`, without
/// namespaces and modules: `["Cart", "Total"]`.
pub fn path(symbols: &[Symbol], index: usize) -> Vec<&str> {
    let mut names = vec![symbols[index].name.as_str()];
    let mut current = symbols[index].parent;
    while let Some(parent) = current {
        let symbol = &symbols[parent];
        if !matches!(symbol.kind, "namespace" | "mod" | "module") {
            names.push(symbol.name.as_str());
        }
        current = symbol.parent;
    }
    names.reverse();
    names
}

/// The innermost declaration whose lines hold `line`.
pub fn enclosing(symbols: &[Symbol], line: usize) -> Option<usize> {
    symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| s.start_line <= line && line <= s.end_line)
        .max_by_key(|(_, s)| s.depth)
        .map(|(index, _)| index)
}

/// What one language declares.
struct Rules {
    /// Declarations with members in their `body`, and what each is called.
    containers: &'static [(&'static str, &'static str)],
    members: &'static [(&'static str, &'static str)],
    /// Nodes looked through: what is inside is declared where they are.
    through: &'static [&'static str],
    /// `export` and `declare` in front of a declaration: part of it.
    wrappers: &'static [&'static str],
    /// Attributes and decorators written before a declaration as siblings.
    lead: &'static [&'static str],
    /// Containers whose members are visible without saying so.
    open: &'static [&'static str],
    /// The same for what is declared outside every container.
    open_at_top: bool,
}

const CSHARP: Rules = Rules {
    containers: &[
        ("namespace_declaration", "namespace"),
        ("class_declaration", "class"),
        ("struct_declaration", "struct"),
        ("record_declaration", "record"),
        ("record_struct_declaration", "record"),
        ("interface_declaration", "interface"),
        ("enum_declaration", "enum"),
    ],
    members: &[
        ("file_scoped_namespace_declaration", "namespace"),
        ("method_declaration", "method"),
        ("constructor_declaration", "constructor"),
        ("destructor_declaration", "destructor"),
        ("operator_declaration", "operator"),
        ("conversion_operator_declaration", "operator"),
        ("property_declaration", "property"),
        ("indexer_declaration", "indexer"),
        ("field_declaration", "field"),
        ("event_field_declaration", "event"),
        ("event_declaration", "event"),
        ("delegate_declaration", "delegate"),
        ("enum_member_declaration", "member"),
        ("local_function_statement", "function"),
    ],
    through: &[
        "declaration_list",
        "global_statement",
        "preproc_if",
        "preproc_elif",
        "preproc_else",
    ],
    wrappers: &[],
    lead: &[],
    open: &["namespace", "interface", "enum"],
    open_at_top: true,
};

const TYPESCRIPT: Rules = Rules {
    containers: &[
        ("class_declaration", "class"),
        ("abstract_class_declaration", "class"),
        ("interface_declaration", "interface"),
        ("enum_declaration", "enum"),
        ("internal_module", "namespace"),
        ("module", "module"),
    ],
    members: &[
        ("function_declaration", "function"),
        ("generator_function_declaration", "function"),
        ("function_signature", "function"),
        ("type_alias_declaration", "type"),
        ("method_definition", "method"),
        ("abstract_method_signature", "method"),
        ("method_signature", "method"),
        ("public_field_definition", "field"),
        ("property_signature", "property"),
        ("index_signature", "index"),
        ("call_signature", "call"),
        ("construct_signature", "constructor"),
        ("enum_assignment", "member"),
        ("lexical_declaration", "const"),
        ("variable_declaration", "const"),
    ],
    through: &["expression_statement"],
    wrappers: &["export_statement", "ambient_declaration"],
    lead: &["decorator"],
    open: &["class", "interface", "enum"],
    open_at_top: false,
};

const RUST: Rules = Rules {
    containers: &[
        ("mod_item", "mod"),
        ("impl_item", "impl"),
        ("trait_item", "trait"),
        ("struct_item", "struct"),
        ("enum_item", "enum"),
        ("union_item", "union"),
    ],
    members: &[
        ("function_item", "fn"),
        ("function_signature_item", "fn"),
        ("const_item", "const"),
        ("static_item", "static"),
        ("type_item", "type"),
        ("associated_type", "type"),
        ("macro_definition", "macro"),
        ("field_declaration", "field"),
        ("enum_variant", "variant"),
    ],
    through: &[],
    wrappers: &[],
    lead: &["attribute_item"],
    open: &["enum", "trait"],
    open_at_top: false,
};

fn rules(language: Language) -> &'static Rules {
    match language {
        Language::CSharp => &CSHARP,
        Language::Rust => &RUST,
        Language::TypeScript | Language::Tsx => &TYPESCRIPT,
    }
}

fn label(table: &[(&str, &'static str)], kind: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == kind).map(|(_, l)| *l)
}

/// Where the declarations being read are.
#[derive(Clone, Copy)]
struct Place {
    depth: usize,
    parent: Option<usize>,
    /// Declarations here are visible unless they say otherwise.
    open: bool,
}

/// One declaration being added.
struct Declared<'t> {
    /// The lines it spans.
    range: Node<'t>,
    /// What holds its name, modifiers and body.
    node: Node<'t>,
    /// `export` or `declare` around it.
    wrapper: Option<Node<'t>>,
    kind: &'static str,
    container: bool,
}

struct Walk<'s> {
    src: &'s [u8],
    language: Language,
    rules: &'static Rules,
    out: Vec<Symbol>,
}

impl<'s> Walk<'s> {
    fn children<'t>(&mut self, node: Node<'t>, place: Place, wrapper: Option<Node<'t>>) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.declare(child, place, wrapper);
        }
    }

    fn declare<'t>(&mut self, node: Node<'t>, place: Place, wrapper: Option<Node<'t>>) {
        let kind = node.kind();
        if let Some(kind_label) = label(self.rules.containers, kind) {
            self.container(node, place, wrapper, kind_label);
        } else if let Some(kind_label) = self.member_label(node) {
            self.member(node, place, wrapper, kind_label);
        } else if self.rules.wrappers.contains(&kind) {
            self.children(node, place, wrapper.or(Some(node)));
        } else if self.rules.through.contains(&kind) {
            self.children(node, place, None);
        }
    }

    fn member<'t>(
        &mut self,
        node: Node<'t>,
        place: Place,
        wrapper: Option<Node<'t>>,
        kind: &'static str,
    ) {
        if matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
            return self.variables(node, place, wrapper);
        }
        self.push(
            Declared {
                range: node,
                node,
                wrapper,
                kind,
                container: false,
            },
            place,
        );
        if node.kind() == "file_scoped_namespace_declaration" {
            // Older grammars put the file's declarations inside it.
            self.children(node, place, None);
        }
    }

    fn member_label(&self, node: Node) -> Option<&'static str> {
        let in_enum = node.parent().is_some_and(|p| p.kind() == "enum_body");
        if node.kind() == "property_identifier" && in_enum {
            return Some("member");
        }
        label(self.rules.members, node.kind())
    }

    fn container<'t>(
        &mut self,
        node: Node<'t>,
        place: Place,
        wrapper: Option<Node<'t>>,
        kind: &'static str,
    ) {
        let index = self.push(
            Declared {
                range: node,
                node,
                wrapper,
                kind,
                container: true,
            },
            place,
        );
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let inside = Place {
            depth: place.depth + 1,
            parent: Some(index),
            open: self.rules.open.contains(&kind) || is_trait_impl(node),
        };
        self.children(body, inside, None);
    }

    /// `const a = 1, b = 2`: one declaration per name. A single name spans
    /// the whole statement.
    fn variables<'t>(&mut self, node: Node<'t>, place: Place, wrapper: Option<Node<'t>>) {
        let mut cursor = node.walk();
        let names: Vec<Node> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        for name in &names {
            let range = if names.len() == 1 { node } else { *name };
            self.push(
                Declared {
                    range,
                    node: *name,
                    wrapper,
                    kind: "const",
                    container: false,
                },
                place,
            );
        }
    }

    fn push(&mut self, declared: Declared, place: Place) -> usize {
        let outer = declared.wrapper.unwrap_or(declared.range);
        let first = self.first_lead(outer);
        let (name, name_byte) = self.name(declared.node);
        let start_line = first.start_position().row + 1;
        self.out.push(Symbol {
            kind: declared.kind,
            name,
            signature: self.signature(outer, &declared),
            start_line,
            end_line: outer.end_position().row + 1,
            doc_line: doc_line(first),
            name_byte,
            depth: place.depth,
            parent: place.parent,
            container: declared.container,
            private: self.is_private(&declared, place),
        });
        self.out.len() - 1
    }

    /// The first of the attributes or decorators right before `node`, else
    /// `node`.
    fn first_lead<'t>(&self, node: Node<'t>) -> Node<'t> {
        let mut first = node;
        while let Some(previous) = first.prev_named_sibling() {
            if !self.rules.lead.contains(&previous.kind()) {
                break;
            }
            first = previous;
        }
        first
    }

    fn name(&self, node: Node) -> (String, usize) {
        let kind = node.kind();
        if let Some(fixed) = label(FIXED_NAMES, kind) {
            return (fixed.to_string(), node.start_byte());
        }
        let (field, before) = NAMED_BY
            .iter()
            .find(|(named, _, _)| *named == kind)
            .map_or(("name", ""), |(_, field, before)| (*field, *before));
        let Some(named) = node.child_by_field_name(field).or_else(|| own_name(node)) else {
            return ("default".to_string(), node.start_byte());
        };
        let text = self.text(named.start_byte(), named.end_byte());
        let text = if kind == "impl_item" {
            bare(&text)
        } else {
            text
        };
        (format!("{before}{text}"), named.start_byte())
    }

    /// The text of the declaration without attributes and without its body;
    /// one that fits on its own line is kept whole.
    fn signature(&self, outer: Node, declared: &Declared) -> String {
        let start = signature_start(outer);
        let end = declared.range.end_byte();
        let one_line = start.start_position().row == declared.range.end_position().row;
        let cut = match body(declared.node) {
            Some(body) if !one_line || declared.container => body.start_byte(),
            _ => end,
        };
        let text = self.text(start.start_byte(), cut.max(start.start_byte()));
        let text = text
            .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | '{'))
            .trim_end_matches("=>")
            .trim_end();
        shorten(text)
    }

    /// Source text with every run of whitespace as one space.
    fn text(&self, start: usize, end: usize) -> String {
        String::from_utf8_lossy(&self.src[start..end])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn is_private(&self, declared: &Declared, place: Place) -> bool {
        match self.access(declared) {
            Some(private) => private,
            None => !place.open,
        }
    }

    /// Whether the declaration says it is private (`Some(true)`), says it is
    /// visible (`Some(false)`), or says nothing.
    fn access(&self, declared: &Declared) -> Option<bool> {
        let words = self.modifiers(declared);
        match self.language {
            Language::CSharp => csharp_access(&words),
            Language::Rust => rust_access(&words, declared.node),
            Language::TypeScript | Language::Tsx => typescript_access(&words, declared),
        }
    }

    /// The access words written on the declaration.
    fn modifiers(&self, declared: &Declared) -> Vec<String> {
        let mut words = Vec::new();
        for holder in [declared.range, declared.node] {
            let mut cursor = holder.walk();
            for child in holder.children(&mut cursor) {
                if matches!(
                    child.kind(),
                    "modifier" | "visibility_modifier" | "accessibility_modifier"
                ) {
                    words.push(self.text(child.start_byte(), child.end_byte()));
                }
            }
        }
        words
    }
}

/// Declarations named by a fixed word.
const FIXED_NAMES: &[(&str, &str)] = &[
    ("indexer_declaration", "this"),
    ("index_signature", "[]"),
    ("call_signature", "()"),
    ("construct_signature", "new"),
];

/// Declarations named by another field than `name`, or with something
/// written before the name: the kind, the field, and what comes before.
const NAMED_BY: &[(&str, &str, &str)] = &[
    ("destructor_declaration", "name", "~"),
    ("operator_declaration", "operator", "operator "),
    ("conversion_operator_declaration", "type", "operator "),
    ("impl_item", "type", ""),
];

/// The name of a declaration that has no `name` field: an enum member that
/// is only a name, or the first name a C# field declares.
fn own_name(node: Node) -> Option<Node> {
    if node.kind() == "property_identifier" {
        return Some(node);
    }
    declarator(node).map(|found| found.child_by_field_name("name").unwrap_or(found))
}

fn has(words: &[String], word: &str) -> bool {
    words.iter().any(|written| written == word)
}

fn csharp_access(words: &[String]) -> Option<bool> {
    if has(words, "private") {
        return Some(!has(words, "protected"));
    }
    ["public", "internal", "protected", "file"]
        .iter()
        .any(|word| has(words, word))
        .then_some(false)
}

/// An `impl` block has no visibility of its own.
fn rust_access(words: &[String], node: Node) -> Option<bool> {
    let visible = node.kind() == "impl_item" || words.iter().any(|w| w.starts_with("pub"));
    visible.then_some(false)
}

fn typescript_access(words: &[String], declared: &Declared) -> Option<bool> {
    let hash_name = declared
        .node
        .child_by_field_name("name")
        .is_some_and(|n| n.kind() == "private_property_identifier");
    if has(words, "private") || hash_name {
        return Some(true);
    }
    let said = ["public", "protected"].iter().any(|word| has(words, word));
    (said || declared.wrapper.is_some()).then_some(false)
}

/// `impl Trait for Type`: its functions are as visible as the trait.
fn is_trait_impl(node: Node) -> bool {
    node.kind() == "impl_item" && node.child_by_field_name("trait").is_some()
}

/// The declarator of a C# field: `int count = 0` holds the name.
fn declarator(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find_map(|child| {
        if child.kind() == "variable_declarator" {
            return Some(child);
        }
        if child.kind() != "variable_declaration" {
            return None;
        }
        let mut inner = child.walk();
        let declarator = child
            .named_children(&mut inner)
            .find(|c| c.kind() == "variable_declarator");
        declarator
    });
    found
}

/// A type name without its type arguments: `Stack<T>` is `Stack`.
fn bare(name: &str) -> String {
    name.split('<').next().unwrap_or(name).trim().to_string()
}

/// Where the body of a declaration starts, if it has one: the block of a
/// function, the members of a type, the accessors of a property, or the
/// body of a function a name is set to.
fn body(node: Node) -> Option<Node> {
    let field = |name: &str| node.child_by_field_name(name);
    field("body").or_else(|| field("accessors")).or_else(|| {
        let value = field("value")?;
        match value.kind() {
            "arrow_function" | "function_expression" | "function" | "generator_function" => {
                value.child_by_field_name("body")
            }
            "arrow_expression_clause" => Some(value),
            _ => None,
        }
    })
}

/// The first part of a declaration that is not an attribute, a decorator
/// or a comment.
fn signature_start(outer: Node) -> Node {
    let mut cursor = outer.walk();
    let first = outer.children(&mut cursor).find(|child| {
        !matches!(child.kind(), "attribute_list" | "decorator") && !child.kind().contains("comment")
    });
    first.unwrap_or(outer)
}

/// The first line of the comments that end right above `node`.
fn doc_line(node: Node) -> usize {
    let mut first = node;
    while let Some(previous) = first.prev_sibling() {
        let touches = previous.end_position().row + 1 >= first.start_position().row;
        if !previous.kind().contains("comment") || !touches {
            break;
        }
        first = previous;
    }
    first.start_position().row + 1
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= SIGNATURE_LIMIT {
        return text.to_string();
    }
    let kept: String = text.chars().take(SIGNATURE_LIMIT).collect();
    format!("{}...", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `depth kind name start-end signature`, one line per declaration; a
    /// private one ends in ` (private)`.
    fn lines(source: &str, language: Language) -> Vec<String> {
        outline(source, language)
            .unwrap()
            .iter()
            .map(|s| {
                format!(
                    "{} {} {} {}-{} | {}{}",
                    s.depth,
                    s.kind,
                    s.name,
                    s.start_line,
                    s.end_line,
                    s.signature,
                    if s.private { " (private)" } else { "" }
                )
            })
            .collect()
    }

    #[test]
    fn csharp_types_and_members_without_bodies() {
        let source = "\
namespace Shop.Orders;

/// <summary>Totals a cart.</summary>
[Serializable]
public sealed class Cart(IPrices prices) : ICart
{
    private readonly List<Item> _items = new();
    public int Count => _items.Count;
    public string Name { get; set; } = \"\";

    [Obsolete]
    public async Task<decimal> Total(
        string currency,
        CancellationToken ct)
    {
        return 0;
    }

    int Hidden() => 1;
}

public interface ICart
{
    int Count { get; }
}

public enum Kind { Small, Large }
public record Point(int X, int Y);
";
        assert_eq!(
            lines(source, Language::CSharp),
            [
                "0 namespace Shop.Orders 1-1 | namespace Shop.Orders",
                "0 class Cart 4-20 | public sealed class Cart(IPrices prices) : ICart",
                "1 field _items 7-7 | private readonly List<Item> _items = new() (private)",
                "1 property Count 8-8 | public int Count => _items.Count",
                "1 property Name 9-9 | public string Name { get; set; } = \"\"",
                "1 method Total 11-17 | public async Task<decimal> Total( string currency, CancellationToken ct)",
                "1 method Hidden 19-19 | int Hidden() => 1 (private)",
                "0 interface ICart 22-25 | public interface ICart",
                "1 property Count 24-24 | int Count { get; }",
                "0 enum Kind 27-27 | public enum Kind",
                "1 member Small 27-27 | Small",
                "1 member Large 27-27 | Large",
                "0 record Point 28-28 | public record Point(int X, int Y)",
            ]
        );
        let symbols = outline(source, Language::CSharp).unwrap();
        // The comment above the attribute is the class's.
        assert_eq!(symbols[1].doc_line, 3);
        assert_eq!(path(&symbols, 5), ["Cart", "Total"]);
        assert_eq!(enclosing(&symbols, 16), Some(5));
        assert_eq!(enclosing(&symbols, 10), Some(1));
        assert_eq!(enclosing(&symbols, 21), None);
    }

    #[test]
    fn typescript_exports_classes_and_signal_members() {
        let source = "\
import { Component, input } from '@angular/core';

const LIMIT = 3;
export const load = async (id: string): Promise<Item> => {
  return fetch(id);
};
export type Item = { id: string };

@Component({ selector: 'app-list' })
export class ListComponent extends Base implements OnInit {
  readonly items = input<Item[]>([]);
  @Output() picked = output<Item>();
  private count = 0;
  #secret = 1;

  constructor(private readonly api: Api) {
    super();
  }

  @HostListener('click')
  onClick(event: Event): void {
    this.count++;
  }
}

export interface Api {
  get(id: string): Promise<Item>;
  readonly base: string;
}

export enum Size { Small, Large = 2 }
function helper() {}
";
        assert_eq!(
            lines(source, Language::TypeScript),
            [
                "0 const LIMIT 3-3 | const LIMIT = 3 (private)",
                "0 const load 4-6 | export const load = async (id: string): Promise<Item>",
                "0 type Item 7-7 | export type Item = { id: string }",
                "0 class ListComponent 9-24 | export class ListComponent extends Base implements OnInit",
                "1 field items 11-11 | readonly items = input<Item[]>([])",
                "1 field picked 12-12 | picked = output<Item>()",
                "1 field count 13-13 | private count = 0 (private)",
                "1 field #secret 14-14 | #secret = 1 (private)",
                "1 method constructor 16-18 | constructor(private readonly api: Api)",
                "1 method onClick 20-23 | onClick(event: Event): void",
                "0 interface Api 26-29 | export interface Api",
                "1 method get 27-27 | get(id: string): Promise<Item>",
                "1 property base 28-28 | readonly base: string",
                "0 enum Size 31-31 | export enum Size",
                "1 member Small 31-31 | Small",
                "1 member Large 31-31 | Large = 2",
                "0 function helper 32-32 | function helper() {} (private)",
            ]
        );
    }

    #[test]
    fn rust_items_impls_and_traits() {
        let source = "\
/// A stack.
#[derive(Debug)]
pub struct Stack<T> {
    items: Vec<T>,
    pub limit: usize,
}

impl<T> Stack<T> {
    pub fn push(&mut self, item: T) {
        self.items.push(item);
    }

    fn grow(&mut self) {}
}

pub trait Shape {
    fn area(&self) -> f64;
}

impl Shape for Stack<u8> {
    fn area(&self) -> f64 {
        0.0
    }
}

pub enum Kind {
    Small,
    Large(u8),
}

mod inner {
    pub const LIMIT: usize = 3;
}
";
        assert_eq!(
            lines(source, Language::Rust),
            [
                "0 struct Stack 2-6 | pub struct Stack<T>",
                "1 field items 4-4 | items: Vec<T> (private)",
                "1 field limit 5-5 | pub limit: usize",
                "0 impl Stack 8-14 | impl<T> Stack<T>",
                "1 fn push 9-11 | pub fn push(&mut self, item: T)",
                "1 fn grow 13-13 | fn grow(&mut self) {} (private)",
                "0 trait Shape 16-18 | pub trait Shape",
                "1 fn area 17-17 | fn area(&self) -> f64",
                "0 impl Stack 20-24 | impl Shape for Stack<u8>",
                "1 fn area 21-23 | fn area(&self) -> f64",
                "0 enum Kind 26-29 | pub enum Kind",
                "1 variant Small 27-27 | Small",
                "1 variant Large 28-28 | Large(u8)",
                "0 mod inner 31-33 | mod inner (private)",
                "1 const LIMIT 32-32 | pub const LIMIT: usize = 3",
            ]
        );
        let symbols = outline(source, Language::Rust).unwrap();
        assert_eq!(symbols[0].doc_line, 1);
        assert_eq!(path(&symbols, 4), ["Stack", "push"]);
        assert_eq!(path(&symbols, 14), ["LIMIT"]);
    }

    #[test]
    fn a_long_signature_is_cut() {
        let values = (0..80).map(|n| format!("\"value{n}\"")).collect::<Vec<_>>();
        let source = format!("const NAMES = [{}];\n", values.join(", "));
        let symbols = outline(&source, Language::TypeScript).unwrap();
        assert!(symbols[0].signature.ends_with("..."));
        assert!(symbols[0].signature.chars().count() <= SIGNATURE_LIMIT + 3);
    }
}

/// One place a name is written in code: not in a comment or a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Use {
    /// Which of the names asked for: an index into them.
    pub name: usize,
    /// 1-based.
    pub line: usize,
    pub byte: usize,
}

/// Every place one of `names` is written as an identifier in `source`, in
/// the order written. By name only: nothing is resolved to a declaration.
pub fn uses(source: &str, language: Language, names: &[&str]) -> Result<Vec<Use>, Error> {
    crate::guarded("<memory>", || {
        let (tree, _) = crate::parse_prepared(source, language, "<memory>")?;
        Ok(written(&tree, source.as_bytes(), names))
    })
}

fn written(tree: &tree_sitter::Tree, src: &[u8], names: &[&str]) -> Vec<Use> {
    let mut found = Vec::new();
    let mut cursor = tree.walk();
    loop {
        found.extend(used(cursor.node(), src, names));
        if !advance(&mut cursor) {
            return found;
        }
    }
}

/// `node` as a use of one of `names`, when it is one.
fn used(node: Node, src: &[u8], names: &[&str]) -> Option<Use> {
    if !is_identifier(node) {
        return None;
    }
    let text = &src[node.start_byte()..node.end_byte()];
    let name = names.iter().position(|name| name.as_bytes() == text)?;
    Some(Use {
        name,
        line: node.start_position().row + 1,
        byte: node.start_byte(),
    })
}

/// Moves to the next node in the order written; false after the last.
fn advance(cursor: &mut tree_sitter::TreeCursor) -> bool {
    if cursor.goto_first_child() {
        return true;
    }
    while !cursor.goto_next_sibling() {
        if !cursor.goto_parent() {
            return false;
        }
    }
    true
}

fn is_identifier(node: Node) -> bool {
    let kind = node.kind();
    node.is_named()
        && node.child_count() == 0
        && (kind.ends_with("identifier") || kind.ends_with("identifier_pattern"))
}

#[cfg(test)]
mod use_tests {
    use super::*;

    fn lines(source: &str, language: Language, names: &[&str]) -> Vec<(usize, usize)> {
        uses(source, language, names)
            .unwrap()
            .iter()
            .map(|u| (u.name, u.line))
            .collect()
    }

    #[test]
    fn names_in_comments_and_strings_are_not_uses() {
        let source = "// Options are read once.
class Options { }
class Startup {
    void Run(Options options) {
        var text = \"Options\";
        Use<Options>(options.Value, nameof(Options));
    }
}
";
        assert_eq!(
            lines(source, Language::CSharp, &["Options", "Run"]),
            [(0, 2), (1, 4), (0, 4), (0, 6), (0, 6)]
        );
    }

    #[test]
    fn typescript_types_properties_and_templates() {
        let source = "import { load } from './load';
export class Page {
  items = load(1);
  show() { return `${this.items} load`; }
}
type Loader = typeof load;
";
        assert_eq!(
            lines(source, Language::TypeScript, &["load", "items"]),
            [(0, 1), (1, 3), (0, 3), (1, 4), (0, 6)]
        );
    }

    #[test]
    fn one_reading_gives_the_declarations_and_the_uses() {
        let source =
            "class Cart { int Total() => 1; }\nclass Shop { int Sum(Cart c) => c.Total(); }\n";
        let (symbols, found) = outline_and_uses(source, Language::CSharp, &["Total"]).unwrap();
        assert_eq!(symbols, outline(source, Language::CSharp).unwrap());
        assert_eq!(found, uses(source, Language::CSharp, &["Total"]).unwrap());
        assert_eq!(found.len(), 2);
        // The first use is where `Total` is declared.
        assert!(symbols.iter().any(|s| s.name_byte == found[0].byte));
        assert!(!symbols.iter().any(|s| s.name_byte == found[1].byte));
    }

    #[test]
    fn rust_paths_fields_and_types() {
        let source = "struct Cart { total: u32 }
fn total(cart: &Cart) -> u32 { cart.total }
";
        assert_eq!(
            lines(source, Language::Rust, &["total", "Cart"]),
            [(1, 1), (0, 1), (0, 2), (1, 2), (0, 2)]
        );
    }
}
