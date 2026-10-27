//! Prints the tree-sitter syntax tree of a file, one named node per line.
//! Handy for finding the node kinds a language analyzer should match.
//!
//!     cargo run -p dog-food-core --example dump -- path/to/File.cs

use std::path::Path;

use tree_sitter::{Node, Parser};

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump <file>");
    let source = std::fs::read_to_string(&path).expect("readable file");
    let grammar: tree_sitter::Language = match Path::new(&path).extension().and_then(|e| e.to_str())
    {
        Some("cs") => tree_sitter_c_sharp::LANGUAGE.into(),
        Some("rs") => tree_sitter_rust::LANGUAGE.into(),
        Some("tsx" | "jsx") => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar).expect("grammar loads");
    let tree = parser.parse(&source, None).expect("parses");
    print(tree.root_node(), None, 0, source.as_bytes());
}

fn print(node: Node, field: Option<&str>, depth: usize, src: &[u8]) {
    if node.is_named() {
        let field = field.map(|f| format!("{f}: ")).unwrap_or_default();
        let text = if node.child_count() == 0 {
            format!("  `{}`", node.utf8_text(src).unwrap_or(""))
        } else {
            String::new()
        };
        println!(
            "{:indent$}{field}{} [{}]{text}",
            "",
            node.kind(),
            node.start_position().row + 1,
            indent = depth * 2
        );
    }
    let mut cursor = node.walk();
    for (i, child) in node.children(&mut cursor).enumerate() {
        let field = node.field_name_for_child(i as u32);
        print(child, field, depth + usize::from(node.is_named()), src);
    }
}
