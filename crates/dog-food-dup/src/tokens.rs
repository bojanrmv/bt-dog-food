//! Source files as blocks of statements, each statement a list of tokens
//! whose names and values are normalized away: two statements with the same
//! shape (keywords, operators, structure) have the same `shape` hash however
//! their variables, members and literals are named.

use std::path::{Path, PathBuf};

use dog_food_core::Language;
use tree_sitter::Node;

/// What a token is, for normalizing and for naming the holes of a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Name,
    Text,
    Number,
    Other,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Kind,
    pub text: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

impl Token {
    /// The token with its name or value taken out.
    fn normalized(&self) -> &str {
        match self.kind {
            Kind::Name => "$name",
            Kind::Text => "$text",
            Kind::Number => "$number",
            Kind::Other => &self.text,
        }
    }
}

/// One statement of a block.
#[derive(Debug, Clone)]
pub struct Statement {
    pub tokens: Vec<Token>,
    /// Hash of the normalized tokens.
    pub shape: u64,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// The statements of one block, in order, and the function they are in.
#[derive(Debug, Clone)]
pub struct Block {
    pub file: usize,
    pub function: Option<String>,
    /// Whether the block is a function's whole body.
    pub is_body: bool,
    pub statements: Vec<Statement>,
}

/// A parsed source file.
pub struct Source {
    pub path: PathBuf,
    /// The text the tree was parsed from: the same length as the file.
    pub text: String,
}

/// The blocks of `path`, numbered as file `index`; `None` for a file of
/// another language or one that does not parse.
pub fn blocks(path: &Path, index: usize, tests: bool) -> Option<(Source, Vec<Block>)> {
    let language = Language::from_path(path)?;
    let source = std::fs::read_to_string(path).ok()?;
    let (tree, text) = dog_food_core::parse_tree(&source, language).ok()?;
    let mut found = Vec::new();
    collect(tree.root_node(), text.as_bytes(), index, tests, &mut found);
    Some((
        Source {
            path: path.to_path_buf(),
            text,
        },
        found,
    ))
}

/// Node kinds that hold a list of statements, in C#, TypeScript and Rust.
const BLOCKS: &[&str] = &["block", "statement_block", "switch_section"];

fn collect(node: Node, src: &[u8], file: usize, tests: bool, out: &mut Vec<Block>) {
    if !tests && is_test_module(node, src) {
        return;
    }
    if BLOCKS.contains(&node.kind()) {
        let mut cursor = node.walk();
        let statements: Vec<Statement> = node
            .named_children(&mut cursor)
            .filter(|c| !is_comment(*c) && !is_label(*c))
            .map(|c| statement(c, src))
            .collect();
        if !statements.is_empty() {
            out.push(Block {
                file,
                function: enclosing_function(node, src),
                is_body: node
                    .parent()
                    .is_some_and(|p| FUNCTIONS.contains(&p.kind()) || p.kind() == "arrow_function"),
                statements,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, src, file, tests, out);
    }
}

/// Rust's `mod tests { ... }` (or one under `#[cfg(test)]`) inside a source file.
fn is_test_module(node: Node, src: &[u8]) -> bool {
    if node.kind() != "mod_item" {
        return false;
    }
    let named_tests = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .is_some_and(|name| name == "tests" || name == "test");
    let cfg_test = node
        .prev_named_sibling()
        .filter(|p| p.kind() == "attribute_item")
        .and_then(|p| p.utf8_text(src).ok())
        .is_some_and(|text| text.replace(' ', "").contains("cfg(test)"));
    named_tests || cfg_test
}

fn is_comment(node: Node) -> bool {
    node.kind().contains("comment")
}

/// The `case x:` labels of a switch section are not statements.
fn is_label(node: Node) -> bool {
    matches!(
        node.kind(),
        "case_switch_label" | "default_switch_label" | "switch_label" | "case_pattern_switch_label"
    )
}

fn statement(node: Node, src: &[u8]) -> Statement {
    let mut tokens = Vec::new();
    leaves(node, src, &mut tokens);
    let parts: Vec<&[u8]> = tokens.iter().map(|t| t.normalized().as_bytes()).collect();
    Statement {
        shape: fnv(&parts),
        tokens,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

/// The leaves under `node`: names, literals (one token each, even a string
/// with parts) and everything else by its text. Comments are left out.
fn leaves(node: Node, src: &[u8], out: &mut Vec<Token>) {
    if is_comment(node) {
        return;
    }
    let kind = token_kind(node);
    if kind != Kind::Other || node.child_count() == 0 {
        let text = node.utf8_text(src).unwrap_or_default().to_string();
        if !text.trim().is_empty() {
            out.push(Token {
                kind,
                text,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
            });
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        leaves(child, src, out);
    }
}

fn token_kind(node: Node) -> Kind {
    let kind = node.kind();
    if kind == "identifier" || kind.ends_with("_identifier") {
        return Kind::Name;
    }
    // A string with interpolations keeps its parts: what is inside matters.
    let interpolated = kind.contains("interpolat") || kind == "template_string";
    if !interpolated
        && (kind.contains("string") || kind == "character_literal" || kind == "char_literal")
    {
        return Kind::Text;
    }
    if matches!(
        kind,
        "integer_literal" | "real_literal" | "float_literal" | "number" | "numeric_literal"
    ) {
        return Kind::Number;
    }
    Kind::Other
}

/// Kinds of function-like nodes, and of the types around them.
const FUNCTIONS: &[&str] = &[
    "method_declaration",
    "constructor_declaration",
    "local_function_statement",
    "function_declaration",
    "method_definition",
    "function_item",
    "accessor_declaration",
];
const TYPES: &[&str] = &[
    "class_declaration",
    "struct_declaration",
    "record_declaration",
    "interface_declaration",
    "class",
    "impl_item",
    "trait_item",
];

/// `Type.Method` (or `Method`) around `node`.
fn enclosing_function(node: Node, src: &[u8]) -> Option<String> {
    let mut current = node.parent();
    let mut function = None;
    let mut owner = None;
    while let Some(n) = current {
        let named = |field: &str| {
            n.child_by_field_name(field)
                .and_then(|c| c.utf8_text(src).ok())
                .map(str::to_string)
        };
        if function.is_none() && FUNCTIONS.contains(&n.kind()) {
            function = named("name");
        } else if owner.is_none() && TYPES.contains(&n.kind()) {
            owner = named("name").or_else(|| named("type"));
        }
        current = n.parent();
    }
    match (owner, function) {
        (Some(owner), Some(function)) => Some(format!("{owner}.{function}")),
        (None, function) => function,
        (Some(owner), None) => Some(owner),
    }
}

pub use dog_food_core::fnv;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_statements_have_the_same_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("A.cs");
        std::fs::write(
            &path,
            "class A {\n  int F(Plan plan) {\n    var total = plan.Price * 2 + 1;\n    var sum = seat.Fee * 3 + 1;\n    var other = seat.Fee * 3 - 1;\n    return total;\n  }\n}\n",
        )
        .unwrap();
        let (_, blocks) = blocks(&path, 0, false).unwrap();
        let block = &blocks[0];
        assert_eq!(block.function.as_deref(), Some("A.F"));
        let shapes: Vec<u64> = block.statements.iter().map(|s| s.shape).collect();
        assert_eq!(shapes[0], shapes[1], "renamed names and values match");
        assert_ne!(shapes[1], shapes[2], "a different operator does not");
        assert_eq!(block.statements[0].start_line, 3);
    }
}
