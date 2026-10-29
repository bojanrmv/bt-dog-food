//! Per-function cyclomatic complexity from source code.
//!
//! Source is parsed with tree-sitter; each language only describes which
//! syntax nodes are functions and which are decision points, and a shared
//! walker does the counting. See `docs/architecture.md`.
//!
//! ```
//! use dog_food_core::{analyze_source, Language};
//!
//! let functions = analyze_source("int Abs(int x) => x < 0 ? -x : x;", Language::CSharp)?;
//! assert_eq!(functions[0].name, "Abs");
//! assert_eq!(functions[0].complexity, 2);
//! # Ok::<(), dog_food_core::Error>(())
//! ```

mod lang;
mod model;
pub mod mutate;
mod walker;

pub use lang::Language;
pub use model::{FileReport, FunctionMetrics};
pub use mutate::Mutant;

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported file type: {0}")]
    Unsupported(String),
    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not load the {0:?} grammar")]
    Grammar(Language),
    #[error("parser gave up on {0}")]
    Parse(String),
    #[error("{0}")]
    Settings(String),
    /// A `dgf-disable` comment that cannot be used, with its 1-based line.
    #[error("line {line}: {message}")]
    Suppression { line: usize, message: String },
}

/// Analyzes source text that is already in memory.
pub fn analyze_source(source: &str, language: Language) -> Result<Vec<FunctionMetrics>, Error> {
    let (functions, _) = analyze(source, language, "<memory>")?;
    Ok(functions)
}

/// Reads and analyzes one file; the language is picked from its extension.
pub fn analyze_file(path: &Path) -> Result<FileReport, Error> {
    let display = path.display().to_string();
    let language = Language::from_path(path).ok_or_else(|| Error::Unsupported(display.clone()))?;
    let source = std::fs::read_to_string(path).map_err(|source| Error::Io {
        path: display.clone(),
        source,
    })?;
    let (functions, has_errors) = analyze(&source, language, &display)?;
    Ok(FileReport {
        path: display,
        language,
        functions,
        has_errors,
    })
}

fn analyze(
    source: &str,
    language: Language,
    origin: &str,
) -> Result<(Vec<FunctionMetrics>, bool), Error> {
    let (tree, text) = parse_prepared(source, language, origin)?;
    // Names come from the text the tree was parsed from: an `extension`
    // block is a class named `extension` there.
    let functions = walker::walk(&tree, text.as_bytes(), language.analyzer());
    Ok((functions, tree.root_node().has_error()))
}

/// FNV-1a, 64 bit, over parts kept apart: stable across Rust versions and
/// machines, unlike std's hasher, so it can key caches on disk.
pub fn fnv(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.iter().chain(&[0xff]) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// The syntax tree of `source`, and the text it was parsed from (`source`,
/// or a same-length form the grammar knows when it predates newer syntax),
/// for tools that work on the tree themselves.
pub fn parse_tree(source: &str, language: Language) -> Result<(tree_sitter::Tree, String), Error> {
    let (tree, text) = parse_prepared(source, language, "<memory>")?;
    Ok((tree, text.into_owned()))
}

/// Parses `source`. When it has syntax errors and the language knows newer
/// syntax its grammar does not (see `lang::prepare`), the source is parsed
/// again in those forms, which keep every byte in place, and the tree with
/// the fewest errors is used.
fn parse(source: &str, language: Language, origin: &str) -> Result<tree_sitter::Tree, Error> {
    Ok(parse_prepared(source, language, origin)?.0)
}

/// The tree, and the text it was parsed from (`source`, or its prepared
/// form): the same length, every node in the same place.
fn parse_prepared<'s>(
    source: &'s str,
    language: Language,
    origin: &str,
) -> Result<(tree_sitter::Tree, std::borrow::Cow<'s, str>), Error> {
    let tree = parse_text(source, language, origin)?;
    let mut best = (errors(&tree), tree, std::borrow::Cow::Borrowed(source));
    for prepared in language.analyzer().prepare(source) {
        if best.0 == 0 {
            break;
        }
        let retry = parse_text(&prepared, language, origin)?;
        let count = errors(&retry);
        if count < best.0 {
            best = (count, retry, prepared.into());
        }
    }
    Ok((best.1, best.2))
}

fn parse_text(source: &str, language: Language, origin: &str) -> Result<tree_sitter::Tree, Error> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language.analyzer().grammar())
        .map_err(|_| Error::Grammar(language))?;
    parser
        .parse(source, None)
        .ok_or_else(|| Error::Parse(origin.to_string()))
}

/// How many error and missing nodes a tree has.
fn errors(tree: &tree_sitter::Tree) -> usize {
    error_nodes(tree.root_node())
}

fn error_nodes(node: tree_sitter::Node) -> usize {
    let own = usize::from(node.is_error() || node.is_missing());
    if !node.has_error() {
        return own;
    }
    let mut cursor = node.walk();
    own + node.children(&mut cursor).map(error_nodes).sum::<usize>()
}
